# README demo

`mural-demo.webp` shows Mural's actual `push:left` and `fade` transitions between
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
downscaled and encoded as animated WebP. Their CC0 dedication is separate
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
- Six seconds captured with `grim` at 25 frames per second. The source capture
  starts and ends on the same landscape, so the loop has no abrupt reset.
- Animated WebP encoded at 640 × 360 and 20 frames per second with FFmpeg:

  ```sh
  ffmpeg -framerate 25 -i frames/%04d.ppm \
    -vf 'fps=20,scale=640:360:flags=lanczos' \
    -c:v libwebp_anim -quality 75 -compression_level 6 \
    -loop 0 mural-demo.webp
  ```

Keep the animation below 1 MB and check playback in the rendered GitHub README,
not just a local decoder. The previous 5.1 MB GIF left a blank area while
GitHub's animation player waited for the download to finish. WebP preserves the
photographs' full color without GIF palette dithering and reduces the download
to about 834 KB.

This documentation capture does not establish hardware or compositor support;
see the [compatibility matrix](../compatibility.md).
