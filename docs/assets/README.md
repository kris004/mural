# README demo

`mural-demo.gif` shows Mural's actual `push:left` and `fade` transitions between
two landscape photographs. It was captured in an isolated headless Sway session,
not from a user's desktop.

## Photo credits

Both photographs are by **Bonnie Moreland**, originally published on ISO
Republic and downloaded from Wikimedia Commons. Both are offered under the
[CC0 1.0 Universal Public Domain Dedication](https://creativecommons.org/publicdomain/zero/1.0/).

| Photograph | Source and license record | Original publisher |
| --- | --- | --- |
| Lake Mountain Landscape | [Wikimedia Commons](https://commons.wikimedia.org/wiki/File:Lake_Mountain_Landscape.jpg) | [ISO Republic](https://isorepublic.com/photo/lake-mountain-landscape/) |
| Meadow Landscape Field | [Wikimedia Commons](https://commons.wikimedia.org/wiki/File:Meadow_Landscape_Field.jpg) | [ISO Republic](https://isorepublic.com/photo/meadow-landscape-field/) |

The photographs were scaled and center-cropped by Mural's `fill` mode, then
downscaled and palette-quantized for the GIF. Their CC0 dedication is separate
from Mural's software licenses.

## Capture details

- Mural 0.1.0, with its normal supervised renderer.
- Sway 1.12, one 800 × 450 headless output at scale 1, using the Pixman
  compositor renderer and software EGL for Mural.
- Dedicated temporary home, configuration, state, cache, runtime directory,
  and IPC sockets; no live desktop session or personal wallpaper library.
- Lake set with `cut`, then meadow with `push:left --mode screen`, then lake
  with `fade`. Both animated transitions use `--duration-ms 1000` and
  `--easing ease-in-out-cubic`.
- Six seconds captured with `grim` at 25 frames per second. The first and last
  frames match, so the loop has no abrupt reset.
- GIF encoded at 640 × 360 and 20 frames per second with FFmpeg:

  ```sh
  ffmpeg -framerate 25 -i frames/%04d.ppm \
    -filter_complex \
    '[0:v]fps=20,scale=640:360:flags=lanczos,split[frames][colors];[colors]palettegen=max_colors=192:stats_mode=full[palette];[frames][palette]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle' \
    -loop 0 mural-demo.gif
  ```

This documentation capture does not establish hardware or compositor support;
see the [compatibility matrix](../compatibility.md).
