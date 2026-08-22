#!/usr/bin/env python3
"""Normalize animated GIFs to a common transparent canvas without jitter."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

from PIL import Image


def load_frames(path: Path):
    frames, durations = [], []
    with Image.open(path) as image:
        loop = int(image.info.get("loop", 0))
        default_duration = int(image.info.get("duration", 100)) or 100
        for index in range(getattr(image, "n_frames", 1)):
            image.seek(index)
            frames.append(image.convert("RGBA").copy())
            durations.append(int(image.info.get("duration", default_duration)) or default_duration)
    return frames, durations, loop


def union_box(frames, alpha_threshold: int):
    union = None
    for frame in frames:
        mask = frame.getchannel("A").point(
            lambda value: 255 if value > alpha_threshold else 0,
            mode="1",
        )
        box = mask.getbbox()
        if box is None:
            continue
        union = box if union is None else (
            min(union[0], box[0]),
            min(union[1], box[1]),
            max(union[2], box[2]),
            max(union[3], box[3]),
        )
    if union is None:
        raise ValueError("GIF contains no visible pixels")
    return union


def metrics(path: Path):
    with Image.open(path) as image:
        duration = 0
        for index in range(getattr(image, "n_frames", 1)):
            image.seek(index)
            duration += int(image.info.get("duration", 100)) or 100
        return image.size, getattr(image, "n_frames", 1), duration


def normalize(source_path: Path, output_path: Path, root: Path, args):
    frames, durations, loop = load_frames(source_path)
    source_canvas = frames[0].size
    left, top, right, bottom = union_box(frames, args.alpha_threshold)
    content_width, content_height = right - left, bottom - top
    scale = min(args.target_height / content_height, args.max_width / content_width)
    width = max(1, round(content_width * scale))
    height = max(1, round(content_height * scale))
    x = (args.canvas - width) // 2
    y = args.canvas - args.bottom_margin - height

    output_frames = []
    for frame in frames:
        cropped = frame.crop((left, top, right, bottom))
        resized = cropped.resize((width, height), Image.Resampling.LANCZOS)
        canvas = Image.new("RGBA", (args.canvas, args.canvas), (0, 0, 0, 0))
        canvas.alpha_composite(resized, (x, y))
        output_frames.append(canvas)

    output_path.parent.mkdir(parents=True, exist_ok=True)
    fd, raw_name = tempfile.mkstemp(prefix="normalize-", suffix=".gif", dir=output_path.parent)
    os.close(fd)
    raw = Path(raw_name)
    optimized = raw.with_name(f"{raw.stem}-optimized.gif")
    try:
        output_frames[0].save(
            raw,
            format="GIF",
            save_all=True,
            append_images=output_frames[1:],
            duration=durations,
            loop=loop,
            disposal=2,
            optimize=False,
        )
        candidate = raw
        if magick := shutil.which("magick"):
            subprocess.run(
                [magick, str(raw), "-coalesce", "-layers", "OptimizeTransparency", "-adjoin", str(optimized)],
                check=True,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
            candidate = optimized
        output_size, output_count, output_duration = metrics(candidate)
        if output_size != (args.canvas, args.canvas):
            raise ValueError(f"unexpected output size for {source_path.name}: {output_size}")
        if output_duration != sum(durations):
            raise ValueError(f"animation duration changed for {source_path.name}")
        os.replace(candidate, output_path)
    finally:
        raw.unlink(missing_ok=True)
        optimized.unlink(missing_ok=True)

    return {
        "file": str(source_path.relative_to(root)),
        "source_frames": len(frames),
        "output_frames": output_count,
        "source_canvas": f"{source_canvas[0]}x{source_canvas[1]}",
        "source_content": f"{content_width}x{content_height}",
        "output_canvas": f"{args.canvas}x{args.canvas}",
        "output_content": f"{width}x{height}",
        "scale": round(scale, 4),
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("character_root", type=Path)
    parser.add_argument("--output-root", type=Path)
    parser.add_argument("--report", required=True, type=Path)
    parser.add_argument("--canvas", type=int, default=512)
    parser.add_argument("--target-height", type=int, default=400)
    parser.add_argument("--max-width", type=int, default=472)
    parser.add_argument("--bottom-margin", type=int, default=32)
    parser.add_argument("--alpha-threshold", type=int, default=8)
    args = parser.parse_args()
    root = args.character_root.resolve()
    output_root = args.output_root.resolve() if args.output_root else root
    files = sorted(root.rglob("*.gif"))
    reports = [
        normalize(path, output_root / path.relative_to(root), root, args)
        for path in files
    ]
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(
        json.dumps({
            "settings": {
                "canvas": args.canvas,
                "target_height": args.target_height,
                "max_width": args.max_width,
                "bottom_margin": args.bottom_margin,
            },
            "files": reports,
        }, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    print(f"Normalized {len(reports)} GIF files")


if __name__ == "__main__":
    main()
