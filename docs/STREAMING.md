# libpgs Streaming Output Reference

## Overview

The `libpgs stream` command extracts PGS (Presentation Graphic Stream) subtitles from MKV and M2TS containers and outputs structured data as newline-delimited JSON (NDJSON) to stdout. Each line is a self-contained JSON object.

This enables any language to consume PGS data incrementally via a subprocess pipe — no temp files, no waiting for full extraction, no PGS format knowledge required.

## Usage

```bash
libpgs stream <file>                      # All tracks
libpgs stream <file> -t 3                 # Single track
libpgs stream <file> -t 3 -t 5            # Multiple tracks
libpgs stream <file> --raw-payloads       # Include base64 raw segment bytes
libpgs stream <file> --start 0:05:00      # From 5 minutes to end of file
libpgs stream <file> --start 0:05:00 --end 0:10:00  # 5-minute window only
```

Timestamps accept `HH:MM:SS.ms`, `MM:SS.ms`, `SS.ms`, or plain seconds (e.g., `300`). When `--start` or `--end` is specified, libpgs seeks directly to the estimated byte offset — data before the start point is not read. If no display sets fall within the range, the stream outputs the tracks header followed by EOF (no error).

Output is flushed after every line. Closing the pipe (e.g., `head -n 10`) causes a clean exit.

## Protocol

The output consists of two types of JSON lines:

1. **Line 1** is always a `tracks` object (track discovery)
2. **All subsequent lines** are `display_set` objects (one per subtitle event)

Check the `"type"` field to distinguish them.

---

## Track Discovery

The first line describes all PGS tracks found in the container.

```json
{
  "type": "tracks",
  "tracks": [
    {
      "track_id": 3,
      "language": "en",
      "container": "Matroska",
      "name": "English Subtitles",
      "is_default": true,
      "is_forced": false,
      "display_set_count": 1234,
      "indexed": true
    }
  ]
}
```

### Track fields

| Field | Type | Description |
|-------|------|-------------|
| `track_id` | `number` | Unique track identifier within the container |
| `language` | `string \| null` | BCP 47 language code (e.g., `"en"`, `"ja"`). Uses ISO 639-1 (2-letter) where available, ISO 639-2/T (3-letter) otherwise. |
| `container` | `string` | Source format: `"Matroska"`, `"M2TS"`, `"TransportStream"`, or `"SUP"` |
| `name` | `string \| null` | Track name from container metadata (MKV TrackName). `null` for M2TS. |
| `is_default` | `boolean \| null` | Whether this track is flagged as default. `null` for M2TS. |
| `is_forced` | `boolean \| null` | Whether this track is flagged as forced. `null` for M2TS. |
| `display_set_count` | `number \| null` | Expected number of display sets (from MKV Tags). `null` if unknown. |
| `indexed` | `boolean \| null` | Whether the container has a seek index for this track, enabling fast random access. `null` for M2TS. |

---

## Display Sets

Each subsequent line represents one display set — a complete subtitle composition event.

### PGS background

A PGS display set defines a single screen update. It contains:
- A **composition** that describes what to show and where (screen dimensions, object placements)
- **Windows** — rectangular screen regions where objects are drawn
- **Palettes** — color lookup tables (YCrCbA format, up to 256 entries)
- **Objects** — RLE-compressed bitmap images

Display sets appear in three states:
- **`epoch_start`** — A completely new display. Contains everything needed to render from scratch.
- **`acquisition_point`** — A refresh point. Contains full replacement data for all objects. Used for mid-stream joining (e.g., seeking into a video).
- **`normal`** — An incremental update. Only contains what changed since the last composition. Commonly used to clear the screen (0 composition objects).

### Full example

```json
{
  "type": "display_set",
  "track_id": 3,
  "index": 42,
  "pts": 92863980,
  "pts_ms": 1031822.0,
  "composition": {
    "number": 430,
    "state": "epoch_start",
    "video_width": 1920,
    "video_height": 1080,
    "palette_only": false,
    "palette_id": 0,
    "objects": [
      {
        "object_id": 0,
        "window_id": 0,
        "x": 773,
        "y": 108,
        "crop": null
      },
      {
        "object_id": 1,
        "window_id": 1,
        "x": 739,
        "y": 928,
        "crop": null
      }
    ]
  },
  "windows": [
    { "id": 0, "x": 773, "y": 108, "width": 377, "height": 43 },
    { "id": 1, "x": 739, "y": 928, "width": 472, "height": 43 }
  ],
  "palettes": [
    {
      "id": 0,
      "version": 0,
      "entries": [
        { "id": 0, "luminance": 16, "cr": 128, "cb": 128, "alpha": 0 },
        { "id": 1, "luminance": 235, "cr": 128, "cb": 128, "alpha": 255 },
        { "id": 2, "luminance": 16, "cr": 128, "cb": 128, "alpha": 255 }
      ]
    }
  ],
  "objects": [
    {
      "id": 0,
      "version": 0,
      "sequence": "complete",
      "data_length": 8635,
      "width": 377,
      "height": 43,
      "bitmap": "<base64 palette indices, 377*43 = 16211 bytes>"
    },
    {
      "id": 1,
      "version": 0,
      "sequence": "complete",
      "data_length": 5210,
      "width": 472,
      "height": 43,
      "bitmap": "<base64 palette indices, 472*43 = 20296 bytes>"
    }
  ]
}
```

---

### Display set fields

| Field | Type | Description |
|-------|------|-------------|
| `type` | `string` | Always `"display_set"` |
| `track_id` | `number` | Matches a `track_id` from the tracks header |
| `index` | `number` | 0-based sequence number, counted per track |
| `pts` | `number` | Presentation timestamp in 90 kHz ticks |
| `pts_ms` | `number` | Presentation timestamp in milliseconds (`pts / 90`) |
| `composition` | `object \| null` | Composition data (from PCS segment). `null` if payload was malformed. |
| `windows` | `array` | Window definitions (from WDS segments). Empty array if none present. |
| `palettes` | `array` | Palette definitions (from PDS segments). Empty array if none present. |
| `objects` | `array` | Object definitions (from ODS segments). Empty array if none present. |

---

### Composition object

The `composition` field contains the presentation composition — the "control plane" of the display set.

| Field | Type | Description |
|-------|------|-------------|
| `number` | `number` | Composition number, incremented per graphics update |
| `state` | `string` | `"epoch_start"`, `"acquisition_point"`, or `"normal"` |
| `video_width` | `number` | Video frame width in pixels (e.g., 1920) |
| `video_height` | `number` | Video frame height in pixels (e.g., 1080) |
| `palette_only` | `boolean` | If `true`, this update only changes the palette — no new objects or positions |
| `palette_id` | `number` | ID of the palette used for this composition |
| `objects` | `array` | Placement instructions — where to draw each object on screen |

#### Composition object placements

Each entry in `composition.objects` is a placement instruction: "draw object X in window Y at position (x, y)."

| Field | Type | Description |
|-------|------|-------------|
| `object_id` | `number` | References an object in the top-level `objects` array by `id` |
| `window_id` | `number` | References a window in the `windows` array by `id` |
| `x` | `number` | Horizontal pixel offset from the top-left corner of the screen |
| `y` | `number` | Vertical pixel offset from the top-left corner of the screen |
| `crop` | `object \| null` | Cropping rectangle, or `null` if not cropped |

#### Crop object (when present)

| Field | Type | Description |
|-------|------|-------------|
| `x` | `number` | Horizontal crop offset within the object |
| `y` | `number` | Vertical crop offset within the object |
| `width` | `number` | Crop width in pixels |
| `height` | `number` | Crop height in pixels |

Cropping is used for progressive subtitle reveal (e.g., showing a few words first, then the rest).

---

### Window definitions

Each entry in `windows` defines a rectangular screen region where objects are drawn.

| Field | Type | Description |
|-------|------|-------------|
| `id` | `number` | Window ID (referenced by `composition.objects[].window_id`) |
| `x` | `number` | Horizontal pixel offset from top-left of screen |
| `y` | `number` | Vertical pixel offset from top-left of screen |
| `width` | `number` | Window width in pixels |
| `height` | `number` | Window height in pixels |

---

### Palette definitions

Each entry in `palettes` defines a color lookup table. Object bitmaps reference palette entries by ID to determine pixel color.

| Field | Type | Description |
|-------|------|-------------|
| `id` | `number` | Palette ID (referenced by `composition.palette_id`) |
| `version` | `number` | Palette version within the current epoch |
| `entries` | `array` | Color entries (up to 256) |

#### Palette entry

Colors are in YCrCb color space with alpha transparency.

| Field | Type | Description |
|-------|------|-------------|
| `id` | `number` | Entry index (0-255). Object bitmap pixels reference this ID. |
| `luminance` | `number` | Luminance / Y component (0-255) |
| `cr` | `number` | Chrominance red (0-255) |
| `cb` | `number` | Chrominance blue (0-255) |
| `alpha` | `number` | Transparency (0 = fully transparent, 255 = fully opaque) |

**Color conversion (YCrCb to RGB):**
```
R = luminance + 1.402 * (cr - 128)
G = luminance - 0.344136 * (cb - 128) - 0.714136 * (cr - 128)
B = luminance + 1.772 * (cb - 128)
```

---

### Object definitions

Each entry in `objects` defines a subtitle image. The RLE-compressed bitmap data is automatically decoded into a flat buffer of palette indices.

| Field | Type | Description |
|-------|------|-------------|
| `id` | `number` | Object ID (referenced by `composition.objects[].object_id`) |
| `version` | `number` | Object version within the current epoch |
| `sequence` | `string` | `"complete"`, `"reassembled"`, `"first"`, `"last"`, or `"continuation"` |
| `data_length` | `number` | Total object data length in bytes (includes 4 bytes for width+height) |
| `width` | `number` | Image width in pixels |
| `height` | `number` | Image height in pixels |
| `bitmap` | `string \| null` | Base64-encoded palette indices (1 byte per pixel, row-major). `null` if decoding failed. |

#### Bitmap format

The `bitmap` field contains the decoded subtitle image as a base64-encoded buffer of palette entry indices. Each byte is an index (0–255) into the `palettes[].entries[]` array. Pixels are stored in row-major order (left to right, top to bottom). The decoded buffer is exactly `width * height` bytes.

To render the image, look up each pixel's palette entry to get its YCrCb color and alpha value. libpgs does not perform color conversion — consumers choose their own color space handling.

#### Object fragmentation

Large objects in the PGS format may be split across multiple ODS segments. libpgs automatically reassembles fragments within each display set and decodes the combined bitmap. Reassembled objects have `"sequence": "reassembled"` to distinguish them from single-segment `"complete"` objects.

| Value | Meaning |
|-------|---------|
| `"complete"` | Single-segment object (most common) |
| `"reassembled"` | Multiple fragments were combined into one object |

With `--raw-payloads`, the `payload` field of a reassembled object contains the concatenated raw payloads of all fragments.

---

## Cross-references

The data model uses ID-based cross-references between sections:

```
composition.objects[].object_id  -->  objects[].id
composition.objects[].window_id  -->  windows[].id
composition.palette_id           -->  palettes[].id
```

A composition object placement says: "draw the bitmap from `objects[id=X]` using colors from `palettes[id=Y]` inside the screen region `windows[id=Z]` at pixel position (x, y)."

---

## Raw payloads (`--raw-payloads`)

By default, only structured data is output. Pass `--raw-payloads` to include the raw PGS segment bytes as base64-encoded strings.

When enabled, each item gains a `"payload"` field:

```json
{
  "composition": { "...": "...", "payload": "<base64>" },
  "windows": [{ "...": "...", "payload": "<base64>" }],
  "palettes": [{ "...": "...", "payload": "<base64>" }],
  "objects": [{ "...": "...", "payload": "<base64>" }]
}
```

The `payload` contains the raw segment payload bytes (after the PGS header). For ODS objects, this includes the RLE-compressed bitmap data. Use this if you need to:
- Write `.sup` files
- Decode RLE bitmaps yourself
- Pass raw data to another PGS-aware tool

If a segment's structured data could not be parsed (malformed payload), the semantic fields will be `null` but the raw `payload` is still included.

---

## Common patterns

### Get subtitle timing

```bash
libpgs stream movie.mkv | jq -r 'select(.type == "display_set") | "\(.pts_ms)ms track=\(.track_id) state=\(.composition.state)"'
```

### Get object positions and sizes

```bash
libpgs stream movie.mkv | jq 'select(.type == "display_set") | .composition.objects[] | {object_id, x, y, window_id}'
```

### Count display sets per track

```bash
libpgs stream movie.mkv | jq -s '[.[] | select(.type == "display_set")] | group_by(.track_id) | map({track: .[0].track_id, count: length})'
```

### Filter epoch starts only

```bash
libpgs stream movie.mkv | jq 'select(.type == "display_set" and .composition.state == "epoch_start")'
```

### Stream a specific time range

```bash
# Get subtitles between 1:30:00 and 1:35:00
libpgs stream movie.mkv --start 1:30:00 --end 1:35:00

# Pipe a 5-minute window to a Python consumer
libpgs stream movie.mkv -t 3 --start 0:05:00 --end 0:10:00 | python process.py
```

### Extract palette colors as RGB

```bash
libpgs stream movie.mkv | jq 'select(.type == "display_set") | .palettes[].entries[] | select(.alpha > 0)'
```

### Render bitmap to image (Python)

```python
import json, base64, sys
from PIL import Image

for line in sys.stdin:
    msg = json.loads(line)
    if msg["type"] != "display_set":
        continue
    palette = msg["palettes"][0]["entries"] if msg["palettes"] else []
    for obj in msg["objects"]:
        if not obj.get("bitmap"):
            continue
        w, h = obj["width"], obj["height"]
        indices = base64.b64decode(obj["bitmap"])
        img = Image.new("RGBA", (w, h))
        for i, idx in enumerate(indices):
            entry = palette[idx] if idx < len(palette) else {"luminance": 0, "cr": 128, "cb": 128, "alpha": 0}
            y_val, cr, cb, a = entry["luminance"], entry["cr"], entry["cb"], entry["alpha"]
            r = max(0, min(255, int(y_val + 1.402 * (cr - 128))))
            g = max(0, min(255, int(y_val - 0.344136 * (cb - 128) - 0.714136 * (cr - 128))))
            b = max(0, min(255, int(y_val + 1.772 * (cb - 128))))
            img.putpixel((i % w, i // w), (r, g, b, a))
        img.save(f"subtitle_{obj['id']}.png")
        break  # first object only
    break  # first display set only
```

---

## Notes

- **Timestamps** use a 90 kHz clock (standard for MPEG transport streams). Divide by 90 to get milliseconds, or use the pre-computed `pts_ms` field.
- **Palette colors** are in YCrCb, not RGB. See the conversion formula above.
- **Up to 2 objects** can be shown simultaneously per composition (e.g., top and bottom subtitle lines), though the PGS spec supports up to 64 per epoch.
- **Normal-state display sets** with 0 composition objects are "clear screen" events — they signal that the previous subtitle should be removed.
- **Palette-only updates** (`palette_only: true`) change colors without replacing objects. The screen content changes appearance but the bitmap data stays the same.
