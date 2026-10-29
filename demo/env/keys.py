"""Draws the keys a tape presses onto its GIF, each with what it does, in a legend in the
bottom right corner while it's pressed (as demo GIFs of nvim-diffmerge show theirs):
`vhs <tape> | python3 demo/env/keys.py <gif> <tape>`.

VHS says each command as it runs it, so the time each is said is when it was pressed,
counted from the tape's `Show` (where the GIF starts), and scaled to the GIF's length:
a frame VHS couldn't take in time it leaves out, so the GIF runs behind the clock.
What a key does is the tape's to
say, on a comment line before it: `#: Attach`; one it says nothing of isn't drawn,
nor is text typed (a prompt), which shows on the screen itself. Keys pressed close
together show together, with the last of their descriptions."""

import os
import re
import subprocess
import sys
import tempfile
import time

BOLD = "/usr/share/fonts/jetbrains-mono/JetBrainsMono-Bold.ttf"
REGULAR = "/usr/share/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf"
SIZE = 17
CELL, LINE = SIZE * 0.6, SIZE * 1.32  # JetBrains Mono: a cell 600/1000 em wide, lines 1320
FILL, KEY, TEXT = "0x3e3a63", "0xf6c177", "0xe0def4"
NAMES = {"Enter": "Enter", "Escape": "Esc", "Backspace": "Backspace", "Tab": "Tab",
         "Space": "Space", "Up": "↑", "Down": "↓", "Left": "←", "Right": "→",
         "PageUp": "PgUp", "PageDown": "PgDn"}
KEYS = re.compile(r"^(Type(@\S+)?|Ctrl\+?|Alt\+?|" + "|".join(NAMES) + r")\b")
TOGETHER = 0.8  # seconds between keys that show as one
SHOWN = 2.2  # seconds a key shows at most, unless the next comes first


def pressed(line):
    """The key a line says it pressed (VHS's `Ctrl Q`, a tape's `Ctrl+Q`), as drawn;
    None for any other."""
    m = re.match(r"^(\w+)(?:@\S+)?[ +]?(.*)$", line.strip())
    if not m:
        return None
    command, rest = m.group(1), m.group(2).strip().strip('"')
    if command == "Type":
        # A key or two (j, mm), one again and again (j ×6), or an editor's
        # command (:wq); not a prompt.
        if len(rest) <= 2:
            return " ".join(rest)
        if len(set(rest)) == 1 and len(rest) <= 9:
            return f"{rest[0]} ×{len(rest)}"
        return rest if rest.startswith(":") and len(rest) <= 6 else None
    if command in ("Ctrl", "Alt"):
        return command + "+" + rest.replace(" ", "+")
    return NAMES.get(command)


def described(tape):
    """What each key command of `tape` (and the tapes it sources, named from where
    VHS runs) does, in order: the `#:` comment before it, or None."""
    out, pending = [], None
    for line in open(tape):
        line = line.strip()
        if line.startswith("#:"):
            pending = line[2:].strip()
        elif line.startswith("Source "):
            out += described(line.split(None, 1)[1])
        elif KEYS.match(line):
            out.append(pending)
            pending = None
    return out


def captions(said, labels, length):
    """(keys, what they do, from, to) of each legend, from (time, line) as VHS said
    them, what the tape says each key does, and the GIF's length in seconds. Keys
    pressed together show one more each as it's pressed."""
    shown = None
    keys = []
    n = 0
    for at, line in said:
        word = line.strip().split(" ", 1)[0]
        if word == "Show":
            shown = at
        elif word == "Hide":
            shown = None
        if not KEYS.match(line.strip()):
            continue
        label = labels[n] if n < len(labels) else None
        n += 1
        key = pressed(line)
        if shown is not None and key:
            keys.append((at - shown, key, label))
    ended = next((at for at, line in said if line.startswith("Creating ")), None)
    first = next((at for at, line in said if line.strip() == "Show"), None)
    scale = length / (ended - first) if ended and first and ended > first else 1.0
    groups = []
    for at, key, label in keys:
        at *= scale
        if groups and at - groups[-1][-1][0] <= TOGETHER and len(groups[-1]) < 4:
            groups[-1].append((at, key, label))
        else:
            groups.append([(at, key, label)])
    out = []
    for g, group in enumerate(groups):
        what = next((label for _, _, label in reversed(group) if label), None)
        if not what:
            continue
        end = group[-1][0] + SHOWN
        if g + 1 < len(groups):
            end = min(end, groups[g + 1][0][0] - 0.05)
        end = max(end, group[-1][0] + 0.6)
        for i, (at, _, _) in enumerate(group):
            upto = group[i + 1][0] if i + 1 < len(group) else end
            out.append(("  ".join(k for _, k, _ in group[: i + 1]), what, at, upto))
    return out


def draw(gif, caps):
    folder = tempfile.mkdtemp()
    filters = []

    def text(name, content):
        path = os.path.join(folder, name)
        with open(path, "w") as f:
            f.write(content)
        return path

    for i, (keys, what, start, end) in enumerate(caps):
        inner = f" {keys}  " + (f"{what} " if what else "")
        n = len(inner)
        on = f":enable='between(t,{start:.3f},{end:.3f})'"
        x, y = f"w-40-{(n + 2) * CELL:.1f}", f"h-24-{3 * LINE:.1f}"
        # The frame as a terminal would draw it (rounded, thin), its line filled.
        frame = text(f"{i}f.txt", "╭" + "─" * n + "╮\n│" + " " * n + "│\n╰" + "─" * n + "╯")
        filters += [
            f"drawtext=fontfile={REGULAR}:textfile={frame}:expansion=none:fontsize={SIZE}:fontcolor={FILL}"
            f":line_spacing=0:y_align=font:x={x}:y={y}{on}",
            f"drawbox=x=iw-40-{(n + 1) * CELL:.1f}:y=ih-24-{2 * LINE:.1f}:w={n * CELL:.1f}"
            f":h={LINE:.1f}:color={FILL}:t=fill{on}",
            f"drawtext=fontfile={BOLD}:textfile={text(f'{i}k.txt', f' {keys}')}:expansion=none:fontsize={SIZE}"
            f":fontcolor={KEY}:y_align=font:x=w-40-{(n + 1) * CELL:.1f}:y=h-24-{2 * LINE:.1f}{on}",
        ]
        if what:
            filters.append(
                f"drawtext=fontfile={REGULAR}:textfile={text(f'{i}w.txt', what)}:expansion=none:fontsize={SIZE}"
                f":fontcolor={TEXT}:y_align=font:x=w-40-{(n + 1 - len(keys) - 3) * CELL:.1f}"
                f":y=h-24-{2 * LINE:.1f}{on}")
    graph = ",".join(filters + ["split[a][b]", "[a]palettegen=stats_mode=diff[p]",
                                "[b][p]paletteuse=dither=none:diff_mode=rectangle"])
    drawn = gif + ".keys.gif"
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", gif, "-filter_complex", graph,
                    drawn], check=True)
    os.replace(drawn, gif)


def main():
    gif, tape = sys.argv[1], sys.argv[2]
    said = []
    for line in sys.stdin:
        said.append((time.time(), line.rstrip("\n")))
        sys.stdout.write(line)
        sys.stdout.flush()
    if not any(line.startswith("Creating ") for _, line in said):
        sys.exit("keys.py: VHS made no GIF")
    labels = described(tape)
    length = float(subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", gif],
        capture_output=True, text=True, check=True).stdout)
    caps = captions(said, labels, length)
    if caps:
        draw(gif, caps)
    print(f"{len(caps)} legends drawn on {gif}")


if __name__ == "__main__":
    main()
