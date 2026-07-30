"""Parakeet / sherpa-onnx sidecar.

Same protocol as the faster-whisper sidecar: one JSON request per line on
stdin, one JSON reply per line on stdout, audio passed as a path to raw
little-endian f32 mono at 16kHz.

sherpa-onnx model directories differ by family, and the quantisation suffix
varies (`encoder.onnx` vs `encoder.int8.onnx`), so the family is detected from
what is actually on disk rather than from the model's name.

Requests:
  {"op":"load","dir":"C:\\...\\sherpa-onnx-nemo-parakeet-..."}
  {"op":"transcribe","dir":"...","pcm":"C:\\...\\x.f32"}
  {"op":"quit"}
"""

import glob
import json
import os
import sys
import time


def reply(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def one(pattern):
    """The single file matching a glob, preferring a quantised build."""
    hits = sorted(glob.glob(pattern))
    if not hits:
        return None
    # int8 first: it is what the catalogue ships and it is markedly faster.
    for h in hits:
        if ".int8." in h:
            return h
    return hits[0]


def build(sherpa_onnx, d, threads, provider):
    """Detect the model family from the files present and build a recogniser."""
    enc = one(os.path.join(d, "encoder*.onnx"))
    dec = one(os.path.join(d, "decoder*.onnx"))
    joi = one(os.path.join(d, "joiner*.onnx"))
    tokens = os.path.join(d, "tokens.txt")

    if enc and dec and joi:
        # NeMo transducer: Parakeet TDT and friends.
        return sherpa_onnx.OfflineRecognizer.from_transducer(
            encoder=enc, decoder=dec, joiner=joi, tokens=tokens,
            num_threads=threads, model_type="nemo_transducer", provider=provider,
        )

    pre = one(os.path.join(d, "preprocess*.onnx"))
    enc2 = one(os.path.join(d, "encode*.onnx"))
    unc = one(os.path.join(d, "uncached_decode*.onnx"))
    cac = one(os.path.join(d, "cached_decode*.onnx"))
    if pre and enc2 and unc and cac:
        return sherpa_onnx.OfflineRecognizer.from_moonshine(
            preprocessor=pre, encoder=enc2, uncached_decoder=unc,
            cached_decoder=cac, tokens=tokens,
            num_threads=threads, provider=provider,
        )

    ctc = one(os.path.join(d, "model*.onnx"))
    if ctc:
        return sherpa_onnx.OfflineRecognizer.from_nemo_ctc(
            model=ctc, tokens=tokens, num_threads=threads, provider=provider,
        )

    raise RuntimeError(
        f"unrecognised model layout in {d}: {sorted(os.listdir(d))[:8]}"
    )


def main():
    try:
        import numpy as np
        import sherpa_onnx
    except Exception as e:  # noqa: BLE001 - surfaced to the Rust side verbatim
        reply({"ok": False, "error": f"import failed: {e}"})
        return

    rec = None
    key = None

    def ensure(d, threads, provider):
        nonlocal rec, key
        want = (d, threads, provider)
        if key != want:
            # No warm-up pass: measured, the first decode is already as fast as
            # the fourth, so ONNX Runtime is not paying a lazy-allocation cost
            # here and a warm-up would only add latency to loading.
            rec = build(sherpa_onnx, d, threads, provider)
            key = want
        return rec

    reply({"ok": True, "ready": True})

    for line in sys.stdin:
        line = line.lstrip("\ufeff").strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            op = req.get("op")
            if op == "quit":
                reply({"ok": True})
                return

            r = ensure(
                req["dir"],
                int(req.get("threads", 4)),
                req.get("provider", "cpu"),
            )
            if op == "load":
                reply({"ok": True})
                continue

            t0 = time.perf_counter()
            pcm = np.fromfile(req["pcm"], dtype=np.float32)
            t1 = time.perf_counter()
            stream = r.create_stream()
            stream.accept_waveform(16000, pcm)
            r.decode_stream(stream)
            t2 = time.perf_counter()
            reply({
                "ok": True,
                "text": stream.result.text.strip(),
                # Split so a slow round trip can be told apart from slow
                # inference, rather than guessed at.
                "read_ms": round((t1 - t0) * 1000, 1),
                "decode_ms": round((t2 - t1) * 1000, 1),
            })

        except Exception as e:  # noqa: BLE001
            reply({"ok": False, "error": str(e)})


if __name__ == "__main__":
    main()
