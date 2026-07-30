"""faster-whisper sidecar.

One JSON request per line on stdin, one JSON reply per line on stdout. Audio
arrives as a path to raw little-endian f32 mono at 16kHz rather than inline:
mixing binary frames with line-delimited JSON on one pipe is a framing bug
waiting to happen, and a temp file costs nothing next to a transcription.

Requests:
  {"op":"load","model":"small.en","device":"auto","compute":"default"}
  {"op":"transcribe","pcm":"C:\\...\\x.f32","language":"en","quick":true}
  {"op":"quit"}

Replies:
  {"ok":true,"text":"..."}          transcribe
  {"ok":true}                        load / quit
  {"ok":false,"error":"..."}
"""

import json
import sys


def reply(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def main():
    try:
        import numpy as np
        from faster_whisper import WhisperModel
    except Exception as e:  # noqa: BLE001 - surfaced to the Rust side verbatim
        reply({"ok": False, "error": f"import failed: {e}"})
        return

    model = None
    key = None

    def ensure(name, device, compute):
        nonlocal model, key
        want = (name, device, compute)
        if key != want:
            # Reloading is expensive, so only do it when something changed.
            model = WhisperModel(name, device=device, compute_type=compute)
            key = want
        return model

    reply({"ok": True, "ready": True})

    for line in sys.stdin:
        # Strip a BOM as well as whitespace. Rust never sends one, but anything
        # piping in from a Windows shell will, and a decode error here would
        # look like a broken engine rather than a broken caller.
        line = line.lstrip("﻿").strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            op = req.get("op")

            if op == "quit":
                reply({"ok": True})
                return

            m = ensure(
                req.get("model", "small.en"),
                req.get("device", "auto"),
                req.get("compute", "default"),
            )

            if op == "load":
                reply({"ok": True})
                continue

            pcm = np.fromfile(req["pcm"], dtype=np.float32)
            segments, _info = m.transcribe(
                pcm,
                language=req.get("language") or None,
                # Interim passes are replaced by the final one, so spending
                # beam search on them only adds latency.
                beam_size=1 if req.get("quick") else 5,
                vad_filter=False,
                condition_on_previous_text=False,
            )
            reply({"ok": True, "text": "".join(s.text for s in segments).strip()})

        except Exception as e:  # noqa: BLE001
            reply({"ok": False, "error": str(e)})


if __name__ == "__main__":
    main()
