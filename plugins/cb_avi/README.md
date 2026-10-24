# Media plugin

`caribou-avi` is xavi's Caribou adapter. It gives Haxe, Wren and Zyntax
programs the `media` API:
- video frames and PCM audio;
- encoders and decoders;
- demuxing and MP4 muxing;
- a player with synchronized audio and video;
- an equalizer.

[xavi](https://github.com/rayzor-blade/xavi) owns the API declaration,
the generators, the native backend over each platform's codecs and audio
output, and the page agent. This crate supplies Caribou's carriers (text,
buffers, enums, futures and errors from `caribou_abi`), and its build
writes:

- the `media` model, from `xavi_bindgen::generate(Runtime::Caribou)`;
- xavi's native and page backends, from `xavi_backend::install`; and
- for a page, the wire and the agent's modules, `xavi.mjs` and
  `xavi_wire.mjs`, which a program ships beside it.

The xavi revision is pinned in the workspace `Cargo.toml`. xavi's README is
the API reference. The package is `media`, as hlavi's is, so code written
against hlavi reads the same.

## Example

```haxe
import haxe.Int64;
import haxe.io.Bytes;
import media.*;

class Main {
    static function main() {
        // A 2 × 2 opaque red RGBA frame.
        var pixels = Bytes.alloc(16);
        for (i in 0...4) {
            pixels.set(i * 4, 255);
            pixels.set(i * 4 + 3, 255);
        }
        var frame = VideoFrame.create(pixels, new VideoFrameBufferInit(
            VideoPixelFormat.RGBA,
            Int64.ofInt(2), Int64.ofInt(2), Int64.ofInt(0)
        ));
        trace(frame.codedWidth());
        frame.close();
    }
}
```

Media handles are released by `close()`, not by the collector: a program
closes what it creates.
