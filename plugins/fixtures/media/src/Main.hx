import haxe.Int64;
import haxe.io.Bytes;
import media.*;

/** The media plugin from Haxe: a video frame made from RGBA bytes and read
	back, and PCM audio through an equalizer. Prints `media ok` when every
	check holds. **/
class Main {
	static function check(what:String, ok:Bool) {
		if (!ok)
			throw 'media: $what';
	}

	static function main() {
		// A 2 × 2 opaque red RGBA frame.
		var pixels = Bytes.alloc(16);
		for (i in 0...4) {
			pixels.set(i * 4, 255);
			pixels.set(i * 4 + 3, 255);
		}
		var frame = VideoFrame.create(pixels, new VideoFrameBufferInit(VideoPixelFormat.RGBA, Int64.ofInt(2), Int64.ofInt(2), Int64.ofInt(0)));
		check("coded width", frame.codedWidth() == Int64.ofInt(2));
		check("coded height", frame.codedHeight() == Int64.ofInt(2));
		check("pixel format", frame.format() == VideoPixelFormat.RGBA);
		frame.close();

		// A quiet 440 Hz mono tone at 48 kHz, filtered.
		var count = 1024;
		var pcm = Bytes.alloc(count * 4);
		for (i in 0...count)
			pcm.setFloat(i * 4, 0.1 * Math.sin(2 * Math.PI * 440 * i / 48000));
		var audio = AudioData.create(new AudioDataInit(AudioSampleFormat.F32, 48000, Int64.ofInt(count), Int64.ofInt(1), Int64.ofInt(0), pcm));
		var eq = AudioEqualizer.create(1);
		eq.setBand(0, 440, 6, 1);
		eq.setPreamp(-6);
		check("band count", eq.bandCount() == 1);
		var filtered = eq.process(audio);
		check("filtered frames", filtered.numberOfFrames() == Int64.ofInt(count));
		filtered.close();
		audio.close();
		eq.close();
		Sys.println("media ok");
	}
}
