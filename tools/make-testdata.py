#!/usr/bin/env python3
"""Generate a synthetic 3-camera shoot for development and tests.

Output: testdata/generated/{master.wav, cam1/, cam2/, cam3/, expected.json}
Each camera has two clips with a gap; the audio in each clip is cut from the
master WAV at a known offset (plus noise and gain), so the sync must recover
the offsets listed in expected.json. Burned-in text shows master time.
"""
import json, math, os, random, struct, subprocess, sys, wave

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "testdata", "generated")
RATE = 48000
DUR = 90.0
FONT = "/usr/share/fonts/truetype/croscore/Arimo-Regular.ttf"

def make_master(path):
    random.seed(1)
    n = int(DUR * RATE)
    env = 0.0
    alpha = 1.0 / (0.12 * RATE)
    # a few "notes" so it sounds like something and has structure
    out = bytearray()
    phase = 0.0
    freq = 110.0
    for i in range(n):
        t = i / RATE
        if i % (RATE // 4) == 0:  # change note every 250 ms
            freq = 110.0 * (2 ** (random.choice([0, 3, 5, 7, 10, 12]) / 12))
        env += alpha * (random.uniform(-1, 1) - env)
        amp = 0.15 + min(1.0, abs(env) * 30)
        phase += 2 * math.pi * freq / RATE
        s = 0.3 * amp * (math.sin(phase) + 0.5 * math.sin(2 * phase)) + 0.15 * amp * random.uniform(-1, 1)
        # a strong click every 10 s makes visual checking easy
        if (i % (10 * RATE)) < 200:
            s += 0.8
        v = max(-1.0, min(1.0, s))
        out += struct.pack("<hh", int(v * 32767), int(v * 32767))
    with wave.open(path, "wb") as w:
        w.setnchannels(2); w.setsampwidth(2); w.setframerate(RATE)
        w.writeframes(bytes(out))

CAMS = {
    # name: (size, fps, rotate, extra video filter, clips [(master_start, duration)])
    "cam1": ("1920x1080", 30, None, "", [(5.0, 30.0), (40.0, 40.0)]),
    "cam2": ("1280x720", 25, "90", "hue=h=120", [(2.5, 47.5), (52.0, 36.0)]),
    "cam3": ("3840x2160", 30, None, "hue=h=240", [(0.0, 30.0), (33.3, 51.7)]),
}

def make_clip(cam, idx, size, fps, rotate, vf, start, dur, master):
    d = os.path.join(ROOT, cam)
    os.makedirs(d, exist_ok=True)
    out = os.path.join(d, f"{cam}_clip{idx}.mp4")
    text = f"{cam} master %{{eif\\:t+{start}\\:d}}.%{{eif\\:(t+{start})*100-floor(t+{start})*100\\:d\\:2}}s"
    filters = [f"testsrc=size={size}:rate={fps}"]
    chain = (vf + "," if vf else "") + f"drawtext=fontfile={FONT}:text='{text}':fontsize=h/8:fontcolor=white:box=1:boxcolor=black@0.6:x=(w-tw)/2:y=h*0.6"
    ct = f"2026-09-01T20:{int(start)//60:02d}:{int(start)%60:02d}.000000Z"
    cmd = ["ffmpeg", "-y", "-hide_banner", "-loglevel", "error",
           "-f", "lavfi", "-i", filters[0],
           "-ss", str(start), "-i", master,
           "-t", str(dur),
           "-filter_complex", f"[0:v]{chain}[v];[1:a]volume=0.7,highpass=f=100[a]",
           "-map", "[v]", "-map", "[a]",
           "-c:v", "libx264", "-preset", "ultrafast", "-crf", "26", "-pix_fmt", "yuv420p",
           "-c:a", "aac", "-b:a", "96k",
           "-metadata", f"creation_time={ct}", "-shortest"]
    if rotate:
        cmd += ["-metadata:s:v:0", f"rotate={rotate}"]
    cmd.append(out)
    subprocess.run(cmd, check=True)
    return out

def main():
    os.makedirs(ROOT, exist_ok=True)
    master = os.path.join(ROOT, "master.wav")
    if not os.path.exists(master):
        make_master(master)
    expected = {}
    for cam, (size, fps, rotate, vf, clips) in CAMS.items():
        for i, (start, dur) in enumerate(clips, 1):
            p = make_clip(cam, i, size, fps, rotate, vf, start, dur, master)
            expected[os.path.basename(p)] = start
            print("wrote", p)
    with open(os.path.join(ROOT, "expected.json"), "w") as f:
        json.dump(expected, f, indent=2)

if __name__ == "__main__":
    main()
