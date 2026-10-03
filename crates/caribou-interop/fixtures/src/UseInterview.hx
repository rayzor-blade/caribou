import game.Prompt;
import game.interview.Interview;

// The Haxe driver of the effects test: it calls into the ZynML module,
// whose `Ask` effect its handler answers through game.Prompt.
class UseInterview {
	static function main() {
		Sys.println(Interview.run());
		Sys.println(Prompt.asked);
		try {
			Interview.refuse();
			Sys.println("no error");
		} catch (e:Dynamic) {
			Sys.println("caught " + e);
		}
	}
}
