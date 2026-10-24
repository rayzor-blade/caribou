import core.Modules;

// A Haxe program that hears module reloads: before a module's new body
// runs, after it ran, and a reload that failed.
class UseNotice {
	static var heard:Array<String> = [];

	static function main() {
		Modules.onReload((namespace, module, before, error) -> {
			heard.push(namespace + ":" + module + (before ? " before" : error == null ? " after" : " failed"));
		});
		Sys.println("listening");
	}

	public static function log():String {
		return heard.join(",");
	}
}
