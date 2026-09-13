package caribou;

#if macro
import haxe.macro.Context;
import haxe.macro.Expr;
import sys.FileSystem;
import sys.io.Process;

/** `caribou::describe::ModuleDesc`, as the runtime prints it. */
private typedef ModuleDesc = {
	lang:String,
	module:String,
	classes:Array<ClassDesc>
}

private typedef ClassDesc = {
	name:String,
	type_name:String,
	?superclass:String,
	members:Array<MemberDesc>
}

private typedef MemberDesc = {
	name:String,
	kind:String,
	signature:String,
	params:Array<{name:String, ty:Dynamic}>,
	ret:Dynamic
}

/** A Wren module found on the classpath and where it lands. */
private typedef Found = {
	path:String,
	namespace:String,
	module:String,
	pack:Array<String>
}
#end

/**
	Makes the other languages' classes available to Haxe code as ordinary
	classes. `-lib caribou` is all a program adds: the library's extra
	params run `caribou.Bridge.use()`, which walks the program's classpath
	for the other languages' modules and emits a class for each class they
	define, under the package the file's path spells, as for a Haxe module:
	`src/game/hud.wren` gives `game.hud.Hud`, so `import game.hud.Hud` and
	`new Hud(3)` are all a program writes. The first directory is the
	namespace the runtime resolves the module through (`game`), and the
	rest is the module's name (`hud`); a file at the classpath root is
	under the language's own namespace. Every member is a native the
	runtime binds by name when the program loads. Names and types come from
	the module's own declarations: for Wren, an
	`#export = "add(n: Num) -> Num"` attribute on a member, and wren_lift's
	inference for a result it can tell.

	The runtime describes its own modules: the `caribou` command, found on
	the path or in the target directory of the checkout this library is
	part of.
**/
class Bridge {
	#if macro
	/** The library every emitted native names. */
	static inline var LIB = "caribou";

	/** The namespace of a module at a classpath root: the language's own. */
	static inline var DEFAULT_NAMESPACE = "wren";

	public static function use():Void {
		var found = [];
		for (cp in Context.getClassPath()) {
			// The project's own paths are relative; the standard library's
			// is absolute.
			if (cp == "" || haxe.io.Path.isAbsolute(cp) || !FileSystem.isDirectory(cp)) {
				continue;
			}
			walk(cp, [], found);
		}
		if (found.length == 0) {
			return;
		}
		var described:Array<ModuleDesc> = haxe.Json.parse(describe(found.map(f -> f.path)));
		for (i in 0...found.length) {
			var f = found[i];
			var doc = described[i];
			for (c in doc.classes) {
				define(f, doc.classes, c);
			}
		}
	}

	static function walk(dir:String, rel:Array<String>, out:Array<Found>):Void {
		for (entry in FileSystem.readDirectory(dir)) {
			if (entry.charAt(0) == ".") {
				continue;
			}
			var path = haxe.io.Path.join([dir, entry]);
			if (FileSystem.isDirectory(path)) {
				walk(path, rel.concat([entry]), out);
			} else if (haxe.io.Path.extension(entry) == "wren") {
				var stem = haxe.io.Path.withoutExtension(entry);
				var segments = rel.concat([stem]);
				var namespace = rel.length > 0 ? rel[0] : DEFAULT_NAMESPACE;
				var module = rel.length > 0 ? rel.slice(1).concat([stem]).join("/") : stem;
				out.push({
					path: path,
					namespace: namespace,
					module: module,
					pack: rel.length > 0 ? segments : [DEFAULT_NAMESPACE].concat(segments)
				});
			}
		}
	}

	/** The `caribou` command: on the path, else the latest built in the
		checkout this library sits in. */
	static function command():String {
		var name = Sys.systemName() == "Windows" ? "caribou.exe" : "caribou";
		for (dir in Sys.getEnv("PATH").split(Sys.systemName() == "Windows" ? ";" : ":")) {
			if (dir != "" && FileSystem.exists(haxe.io.Path.join([dir, name]))) {
				return name;
			}
		}
		var here = haxe.io.Path.directory(Context.resolvePath("caribou/Bridge.hx"));
		var latest = null;
		var latestTime = 0.0;
		for (profile in ["debug", "release"]) {
			var built = haxe.io.Path.join([here, "..", "..", "target", profile, name]);
			if (FileSystem.exists(built)) {
				var time = FileSystem.stat(built).mtime.getTime();
				if (latest == null || time > latestTime) {
					latest = built;
					latestTime = time;
				}
			}
		}
		if (latest == null) {
			Context.fatalError("the caribou command is not on the path", Context.currentPos());
		}
		return latest;
	}

	/** Ask the runtime to describe the modules. */
	static function describe(paths:Array<String>):String {
		var exe = command();
		var p = new Process(exe, ["describe"].concat(paths));
		var out = p.stdout.readAll().toString();
		var err = p.stderr.readAll().toString();
		var code = p.exitCode();
		p.close();
		if (code != 0) {
			Context.fatalError('$exe --describe failed: $err', Context.currentPos());
		}
		return out;
	}

	/** A registry type as a Haxe type. An object type is the class of the
		same module that reports it, else `Dynamic`. */
	static function haxeType(ty:Dynamic, pack:Array<String>, classes:Array<ClassDesc>):ComplexType {
		if (Std.isOfType(ty, String)) {
			return switch ((ty : String)) {
				case "Float": macro :Float;
				case "Int": macro :Int;
				case "Bool": macro :Bool;
				case "Str": macro :String;
				case "Void": macro :Void;
				default: macro :Dynamic;
			}
		}
		if (Reflect.hasField(ty, "Object")) {
			var typeName:String = Reflect.field(ty, "Object");
			for (c in classes) {
				if (c.type_name == typeName) {
					return TPath({pack: pack, name: c.name});
				}
			}
			return macro :Dynamic;
		}
		if (Reflect.hasField(ty, "Array")) {
			return macro :Array<Dynamic>;
		}
		return macro :Dynamic;
	}

	/** The class's members with its superclasses' from the same module
		behind them: each emitted class stands alone under `caribou.Ref`,
		so what it inherits it declares. A member the class defines hides
		a superclass's of the same name and kind. */
	static function flattened(c:ClassDesc, classes:Array<ClassDesc>):Array<MemberDesc> {
		var members = c.members.copy();
		var seen = [for (m in members) m.kind + ":" + m.signature => true];
		var parent = c.superclass;
		var depth = 0;
		while (parent != null && depth++ < classes.length) {
			var p = Lambda.find(classes, x -> x.name == parent);
			if (p == null) {
				break;
			}
			for (m in p.members) {
				// Constructors are the class's own.
				if (m.kind == "constructor" || m.kind == "factory") {
					continue;
				}
				var key = m.kind + ":" + m.signature;
				if (!seen.exists(key)) {
					seen.set(key, true);
					members.push(m);
				}
			}
			parent = p.superclass;
		}
		return members;
	}

	static function define(f:Found, classes:Array<ClassDesc>, c:ClassDesc):Void {
		var pos = Context.currentPos();
		var pack = f.pack;
		var self = TPath({pack: pack, name: c.name});
		var prefix = '${f.namespace}:${f.module}.${c.name}.';
		var fields:Array<Field> = [];
		var taken = new Map<String, Bool>();
		var properties = new Map<String, {get:Bool, set:Bool, type:ComplexType}>();
		var hasConstructor = false;

		function native(symbol:String, args:Array<FunctionArg>, ret:ComplexType, name:String):String {
			// The body is never run: genhl emits the native in its place.
			var body = haxe.macro.ComplexTypeTools.toString(ret) == "Void" ? macro {} : macro return null;
			fields.push({
				name: name,
				pos: pos,
				access: [AStatic],
				meta: [{name: ":hlNative", params: [macro $v{LIB}, macro $v{symbol}], pos: pos}],
				kind: FFun({args: args, ret: ret, expr: body})
			});
			return name;
		}

		function dynamicArgs(m:MemberDesc):Array<FunctionArg> {
			return [for (p in m.params) {name: p.name, type: macro :Dynamic}];
		}

		function typedArgs(m:MemberDesc):Array<FunctionArg> {
			return [for (p in m.params) {name: p.name, type: haxeType(p.ty, pack, classes)}];
		}

		function unique(name:String, arity:Int):String {
			var candidate = name;
			if (taken.exists(candidate)) {
				candidate = name + arity;
			}
			var n = 2;
			while (taken.exists(candidate)) {
				candidate = name + arity + "_" + n++;
			}
			taken.set(candidate, true);
			return candidate;
		}

		for (m in flattened(c, classes)) {
			var arity = m.params.length;
			var callArgs = [for (p in m.params) macro $i{p.name}];
			var ret = haxeType(m.ret, pack, classes);
			// The native's Haxe name: distinct per kind, name and arity.
			var nativeName = "__" + m.kind + "_" + m.name + arity;
			switch (m.kind) {
				case "constructor" if (!hasConstructor):
					// `new Hud(3)`: the native makes the foreign object for
					// the fresh Haxe one and binds the two.
					hasConstructor = true;
					var init = native(prefix + "construct:" + m.signature, [{name: "self", type: self}].concat(dynamicArgs(m)), macro :Void, nativeName);
					var call = [macro this].concat(callArgs);
					fields.push({
						name: "new",
						pos: pos,
						access: [APublic],
						kind: FFun({args: typedArgs(m), ret: null, expr: macro {
							super();
							$i{init}($a{call});
						}})
					});
				case "factory", "static":
					var isProperty = m.signature.indexOf("(") < 0;
					var target = native(prefix + "static:" + m.signature, dynamicArgs(m), macro :Dynamic, nativeName);
					var name = unique(m.name, arity);
					if (isProperty) {
						fields.push({
							name: name,
							pos: pos,
							access: [APublic, AStatic],
							kind: FProp("get", "never", ret)
						});
						fields.push({
							name: "get_" + name,
							pos: pos,
							access: [AStatic, AInline],
							kind: FFun({args: [], ret: ret, expr: macro return $i{target}()})
						});
					} else {
						fields.push({
							name: name,
							pos: pos,
							access: [APublic, AStatic, AInline],
							kind: FFun({args: typedArgs(m), ret: ret, expr: macro return $i{target}($a{callArgs})})
						});
					}
				case "method":
					var target = native(prefix + m.signature, [{name: "self", type: self}].concat(dynamicArgs(m)), macro :Dynamic, nativeName);
					var name = unique(m.name, arity);
					var call = [macro this].concat(callArgs);
					fields.push({
						name: name,
						pos: pos,
						access: [APublic, AInline],
						kind: FFun({args: typedArgs(m), ret: ret, expr: macro return $i{target}($a{call})})
					});
				case "getter":
					var target = native(prefix + m.signature, [{name: "self", type: self}], macro :Dynamic, nativeName);
					var p = properties.get(m.name);
					if (p == null) {
						properties.set(m.name, p = {get: false, set: false, type: ret});
					}
					p.get = true;
					p.type = ret;
					fields.push({
						name: "get_" + m.name,
						pos: pos,
						access: [AInline],
						kind: FFun({args: [], ret: ret, expr: macro return $i{target}(this)})
					});
				case "setter":
					var target = native(prefix + m.signature, [{name: "self", type: self}, {name: "value", type: macro :Dynamic}], macro :Void, nativeName);
					var valueType = typedArgs(m)[0].type;
					var p = properties.get(m.name);
					if (p == null) {
						properties.set(m.name, p = {get: false, set: false, type: valueType});
					}
					p.set = true;
					fields.push({
						name: "set_" + m.name,
						pos: pos,
						access: [AInline],
						kind: FFun({args: [{name: "value", type: valueType}], ret: valueType, expr: macro {
							$i{target}(this, value);
							return value;
						}})
					});
				default:
			}
		}
		for (name => p in properties) {
			taken.set(name, true);
			fields.push({
				name: name,
				pos: pos,
				access: [APublic],
				kind: FProp(p.get ? "get" : "never", p.set ? "set" : "never", p.type)
			});
		}
		if (!hasConstructor) {
			fields.push({
				name: "new",
				pos: pos,
				access: [],
				kind: FFun({args: [], ret: null, expr: macro super()})
			});
		}
		Context.defineType({
			pack: pack,
			name: c.name,
			pos: pos,
			meta: [{name: ":keep", pos: pos}],
			kind: TDClass({pack: ["caribou"], name: "Ref"}),
			fields: fields
		});
	}
	#end
}
