//! Self-update from GitHub Releases.
//!
//! Verba ships as a single portable exe rather than an installer, so Tauri's
//! updater plugin does not fit: it expects to hand an MSI or NSIS package to
//! Windows, and it wants a signing keypair and a hosted manifest. What a
//! portable binary needs instead is the oldest trick on Windows — a running
//! image cannot be overwritten, but it *can* be renamed, so the new exe is
//! moved into place beside the old one and the process restarts into it.
//!
//! ## What this does and does not protect against
//!
//! The download is verified against the SHA-256 GitHub reports for the asset,
//! over HTTPS, from a repository hardcoded below. That catches a truncated or
//! corrupted transfer and a tampered CDN response. It does **not** protect
//! against a compromised GitHub account: the digest and the file come from the
//! same place, so whoever can replace one can replace the other. Defending
//! against that needs a signature made with a key that never touches GitHub,
//! which is a key-management problem this project does not have an answer for
//! yet. The README says so plainly rather than implying more than is true.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const EVENT: &str = "verba:update";

const REPO: &str = "SambuddhaRoy/Verba";
/// The release asset that *is* the application. A release carrying anything
/// else — checksums, source archives — is ignored.
const ASSET: &str = "Verba.exe";

/// GitHub rejects API requests without one.
const UA: &str = concat!("Verba/", env!("CARGO_PKG_VERSION"), " (self-updater)");

/// Long enough for a slow connection to answer, short enough that a hung
/// endpoint does not keep a thread parked for the session.
const API_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

// --- versions -------------------------------------------------------------

/// A three-part version, compared numerically.
///
/// Derived `Ord` on the fields in order is exactly major-then-minor-then-patch,
/// which is the point: comparing the strings instead would make 0.10.0 older
/// than 0.9.0 and silently strand everyone on the earlier release.
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Copy)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// Accepts `1.2.3` and `v1.2.3`, and tolerates a `-suffix` on the last part
    /// by ignoring it. Missing parts are zero, so `v1` is 1.0.0.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
        // A pre-release or build suffix is dropped rather than ordered. Verba
        // does not publish them, and `releases/latest` filters them out anyway;
        // treating one as its base version is better than refusing to parse.
        let s = s.split(['-', '+']).next()?;

        let mut it = s.split('.');
        let mut part = || -> Option<u32> {
            match it.next() {
                None => Some(0),
                Some(p) => p.parse().ok(),
            }
        };
        let v = Version(part()?, part()?, part()?);
        // Anything left over means this was not a version at all.
        if it.next().is_some() {
            return None;
        }
        Some(v)
    }
}

/// The version this binary was built as.
pub fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("CARGO_PKG_VERSION is set by cargo and always parses")
}

// --- checking -------------------------------------------------------------

/// Deserialize as well as Serialize: the settings window hands this straight
/// back to `download_update` after the user agrees to it.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Available {
    pub version: String,
    pub url: String,
    pub size: u64,
    /// Lowercase hex, no `sha256:` prefix.
    pub sha256: String,
    pub notes: String,
}

#[derive(serde::Deserialize)]
struct Release {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(serde::Deserialize)]
struct Asset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    browser_download_url: String,
    /// `sha256:<hex>`. Absent on releases published before GitHub added it, in
    /// which case the update is refused rather than installed unverified.
    #[serde(default)]
    digest: Option<String>,
}

/// Ask GitHub for the newest release. `Ok(None)` means we are already current.
///
/// The `latest` endpoint excludes drafts and pre-releases, so a tagged beta
/// never reaches anyone automatically.
pub fn check() -> Result<Option<Available>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let rel: Release = ureq::get(&url)
        .header("User-Agent", UA)
        .header("Accept", "application/vnd.github+json")
        .config()
        .timeout_global(Some(API_TIMEOUT))
        .build()
        .call()
        .map_err(|e| format!("cannot reach GitHub: {e}"))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("unreadable release feed: {e}"))?;

    let latest = Version::parse(&rel.tag_name)
        .ok_or_else(|| format!("release tag {:?} is not a version", rel.tag_name))?;

    if latest <= current() {
        return Ok(None);
    }

    let asset = rel
        .assets
        .into_iter()
        .find(|a| a.name.eq_ignore_ascii_case(ASSET))
        .ok_or_else(|| format!("release {} has no {ASSET}", rel.tag_name))?;

    // No digest means no way to tell the real download from a substituted one.
    // Refusing is the only honest outcome; the user can still update by hand.
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .ok_or("release asset has no SHA-256 digest; refusing to update automatically")?
        .to_ascii_lowercase();

    Ok(Some(Available {
        version: rel.tag_name.trim_start_matches(['v', 'V']).to_string(),
        url: asset.browser_download_url,
        size: asset.size,
        sha256,
        notes: rel.body,
    }))
}

// --- staging --------------------------------------------------------------

fn exe_dir() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map_err(|e| format!("cannot locate the running exe: {e}"))?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "the running exe has no parent directory".to_string())
}

/// Where the replacement is downloaded to, and where the outgoing exe is
/// parked. Both sit beside the running binary on purpose: `rename` is only
/// atomic within a volume, and the app data folder can easily be on another
/// drive from wherever the user dropped the portable exe.
fn staged_path() -> Result<PathBuf, String> {
    Ok(exe_dir()?.join("Verba.exe.new"))
}
fn retired_path() -> Result<PathBuf, String> {
    Ok(exe_dir()?.join("Verba.exe.old"))
}

/// Download the new binary next to the current one and verify it.
///
/// Reports `(received, total)` so the settings window can draw a bar.
pub fn stage<F: FnMut(u64, u64)>(avail: &Available, mut on_progress: F) -> Result<PathBuf, String> {
    let staged = staged_path()?;
    // Fail early and legibly. A portable exe dropped in Program Files cannot
    // update itself, and finding that out after a 70MB download is worse than
    // finding it out now.
    writable(&exe_dir()?)?;

    let part = staged.with_extension("new.part");
    let result = (|| -> Result<(), String> {
        let resp = ureq::get(&avail.url)
            .header("User-Agent", UA)
            .call()
            .map_err(|e| format!("download failed: {e}"))?;

        let mut reader = resp.into_body().into_reader();
        let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();

        let total = avail.size;
        let mut buf = vec![0u8; 256 * 1024];
        let mut received = 0u64;
        let mut last_report = 0u64;

        loop {
            let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            hasher.update(&buf[..n]);
            received += n as u64;

            let step = (total / 100).max(1 << 20);
            if received - last_report >= step {
                last_report = received;
                on_progress(received, total);
            }
        }
        out.flush().map_err(|e| e.to_string())?;
        drop(out);

        if total > 0 && received != total {
            return Err(format!("truncated: got {received} of {total} bytes"));
        }

        let got = hasher.finish_hex();
        if got != avail.sha256 {
            return Err(format!(
                "checksum mismatch: expected {}, got {got}",
                avail.sha256
            ));
        }

        std::fs::rename(&part, &staged).map_err(|e| e.to_string())?;
        Ok(())
    })();

    if let Err(e) = result {
        // Never leave something that looks like a verified update behind.
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }
    Ok(staged)
}

/// Can we create and delete a file here? Checking the read-only attribute is
/// not enough — ACLs and virtualisation both deny writes to a directory that
/// looks perfectly writable.
fn writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(".verba-write-test");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(format!(
            "cannot write to {} ({e}). Move Verba somewhere writable, \
             or download the new version manually.",
            dir.display()
        )),
    }
}

/// True when a verified replacement is sitting ready.
pub fn staged() -> bool {
    staged_path().map(|p| p.is_file()).unwrap_or(false)
}

// --- applying -------------------------------------------------------------

/// Swap the staged binary in and start it. The caller exits immediately after.
///
/// Windows will not let a running image be deleted or overwritten, but it will
/// let it be renamed, which is what makes this possible at all.
pub fn apply() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let staged = staged_path()?;
    let retired = retired_path()?;

    if !staged.is_file() {
        return Err("no staged update".into());
    }

    // A leftover from an earlier update would block the rename below.
    let _ = std::fs::remove_file(&retired);

    std::fs::rename(&exe, &retired)
        .map_err(|e| format!("cannot move the running exe aside: {e}"))?;

    if let Err(e) = std::fs::rename(&staged, &exe) {
        // Put it back. Leaving the directory with no Verba.exe at all would
        // turn a failed update into a lost application.
        let _ = std::fs::rename(&retired, &exe);
        return Err(format!("cannot move the new exe into place: {e}"));
    }

    // Detached from our stdio on purpose. The child inherits the console
    // handles otherwise, and a shell that ran `--self-update` then sits there
    // waiting on a pipe the tray app never closes — it looks like the update
    // hung when it had already finished.
    std::process::Command::new(&exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("updated, but could not restart: {e}"))?;
    Ok(())
}

/// Delete the previous binary, left behind by the update that replaced it.
/// Called at startup, because that is the first moment it is no longer running.
pub fn sweep() {
    if let Ok(old) = retired_path() {
        if old.is_file() && std::fs::remove_file(&old).is_ok() {
            crate::log!("removed the previous version");
        }
    }
}

// --- sha-256 --------------------------------------------------------------

/// A minimal SHA-256, so verifying a download does not pull in a crate.
///
/// FIPS 180-4. The constants are the first 32 bits of the fractional parts of
/// the square roots (H) and cube roots (K) of the first primes; they are
/// checked against the standard's own test vectors below, which is the only
/// reason it is reasonable to hand-roll this at all.
struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    len: usize,
    total: u64,
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    fn new() -> Self {
        Sha256 {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buf: [0; 64],
            len: 0,
            total: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        while !data.is_empty() {
            let take = (64 - self.len).min(data.len());
            self.buf[self.len..self.len + take].copy_from_slice(&data[..take]);
            self.len += take;
            data = &data[take..];
            if self.len == 64 {
                let block = self.buf;
                self.block(&block);
                self.len = 0;
            }
        }
    }

    fn block(&mut self, b: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b_, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b_) ^ (a & c) ^ (b_ & c);
            let t2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b_;
            b_ = a;
            a = t1.wrapping_add(t2);
        }
        for (dst, v) in self.h.iter_mut().zip([a, b_, c, d, e, f, g, h]) {
            *dst = dst.wrapping_add(v);
        }
    }

    fn finish_hex(mut self) -> String {
        let bits = self.total.wrapping_mul(8);
        self.update(&[0x80]);
        while self.len != 56 {
            self.update(&[0]);
        }
        // update() has been maintaining `total`, so stamp the real length in
        // directly rather than letting the padding count towards it.
        let block_len = self.len;
        self.buf[block_len..block_len + 8].copy_from_slice(&bits.to_be_bytes());
        let block = self.buf;
        self.block(&block);

        self.h.iter().map(|w| format!("{w:08x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Sha256, Version};

    /// Lexical comparison would put 0.10.0 before 0.9.0 and strand every user
    /// on the older release, which is the bug this type exists to prevent.
    #[test]
    fn versions_compare_numerically() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("0.10.0") > v("0.9.0"), "double-digit minor must win");
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.1.1") > v("0.1.0"));
        assert!(v("v0.2.0") > v("0.1.9"), "a v prefix must not change ordering");
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        assert_eq!(v("1"), Version(1, 0, 0), "missing parts are zero");
        assert_eq!(v("0.1.0-beta.2"), v("0.1.0"), "a suffix is dropped");
    }

    /// An unparseable tag must not be treated as version zero — that would make
    /// every release look newer than it is and trigger an endless update loop.
    #[test]
    fn nonsense_tags_are_rejected() {
        for s in ["", "latest", "v", "1.2.3.4", "one.two.three", "0.x.0"] {
            assert!(Version::parse(s).is_none(), "{s:?} must not parse");
        }
    }

    /// The same release must never look like an upgrade, or the app would
    /// download and restart into itself forever.
    #[test]
    fn equal_and_older_are_not_upgrades() {
        let cur = super::current();
        assert!(!(cur > cur));
        let older = Version(cur.0, cur.1, cur.2.saturating_sub(1));
        assert!(older <= cur);
    }

    fn hex(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        h.finish_hex()
    }

    /// FIPS 180-4 test vectors. A hash that is subtly wrong would reject every
    /// legitimate update while looking like a working integrity check.
    #[test]
    fn sha256_matches_the_standard_vectors() {
        assert_eq!(
            hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// Multi-block input with an awkward split: the buffering in update() is
    /// where a hand-rolled hash usually goes wrong, and a mistake there only
    /// shows up on large files — which every real update is.
    #[test]
    fn sha256_is_independent_of_chunking() {
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let one = hex(&data);

        for chunk in [1usize, 63, 64, 65, 1000, 65536] {
            let mut h = Sha256::new();
            for part in data.chunks(chunk) {
                h.update(part);
            }
            assert_eq!(h.finish_hex(), one, "chunked by {chunk} must match");
        }
    }

    /// A million 'a' characters — the standard's long vector, which exercises
    /// the 64-bit length field and many blocks.
    #[test]
    fn sha256_long_vector() {
        let mut h = Sha256::new();
        for _ in 0..1000 {
            h.update(&[b'a'; 1000]);
        }
        assert_eq!(
            h.finish_hex(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }
}
