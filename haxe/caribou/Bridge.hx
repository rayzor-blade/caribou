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
	classes:Array<ClassDesc>,
	?enums:Array<{name:String, variants:Array<{name:String, fields:Array<{name:String, ty:Dynamic}>}>}>,
	?path:String,
	/** The functions the module itself owns. Haxe imports types, so
		these are not emitted: a class the module declares is what Haxe
		reaches. */
	?functions:Array<MemberDesc>
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
	`new Hud(3)` are all a program writes; `src/game/scorer.zynml`, with
	the ZynML snapshot at the root, gives `game.scorer.Scorer` the same
	way. The first directory is the namespace the runtime resolves the
	module through (`game`), and the rest is the module's name (`hud`); a
	file at the classpath root is under the language's own namespace.
	Every member is a native the runtime binds by name when the program
	loads. Names and types come from the module's own declarations: for
	Wren, an `#export = "add(n: Num) -> Num"` attribute on a member, and
	wren_lift's inference for a result it can tell; for a Zyntax language,
	the module's HIR.

	The runtime describes its own modules: `caribou describe <root>` on
	each classpath, the command found on the path or in the target
	directory of the checkout this library is part of.
**/
class Bridge {
	#if macro
	/** The library every emitted native names. */
	static inline var LIB = "caribou";

	/** Every module found, for a type that names a class of another. */
	static var modules:Array<Found> = [];

	/** Plugin object identities span their per-class modules. */
	static var pluginTypes:Map<String, TypePath> = [];

	public static function use():Void {
		var found:Array<Found> = [];
		var docs:Array<ModuleDesc> = [];
		for (cp in Context.getClassPath()) {
			// The project's own paths are relative; the standard library's
			// is absolute.
			if (cp == "" || haxe.io.Path.isAbsolute(cp) || !FileSystem.isDirectory(cp)) {
				continue;
			}
			var described:Array<ModuleDesc> = haxe.Json.parse(describe([cp]));
			for (doc in described) {
				found.push(place(cp, doc));
				docs.push(doc);
			}
		}
		var plugins = pluginLibraries();
		if (found.length == 0 && plugins.length == 0) {
			return;
		}
		// The adapter accesses these fields natively, even when Haxe only
		// sees Bytes in a generated signature and would remove its fields.
		for (field in ["length", "b"]) {
			haxe.macro.Compiler.addGlobalMetadata("haxe.io.Bytes." + field, "@:keep", false, false, true);
		}
		modules = found.copy();
		var described:Array<ModuleDesc> = plugins.length == 0 ? [] : haxe.Json.parse(describe(plugins));
		pluginTypes = [];
		// Resolve signatures against the whole plugin catalog, before
		// emitting any class. A return type can name a later module and
		// must stay typed even when the caller never imports that class.
		for (doc in described) {
			modules.push({path: "", namespace: doc.lang, module: doc.module, pack: [doc.lang]});
			for (c in doc.classes) {
				pluginTypes.set(c.type_name, {pack: [doc.lang], name: c.name});
			}
		}
		for (i in 0...found.length) {
			for (c in docs[i].classes) {
				define(found[i], docs[i].classes, c);
			}
		}
		// A plugin is a language of its own, named after it, with one
		// module per class: `plugins/math.dylib` beside the program gives
		// `math.Vec2`.
		if (plugins.length > 0) {
			for (doc in described) {
				if (doc.enums != null) for (e in doc.enums) {
					var parts = e.name.split(".");
					var name = parts.pop();
					var pos = Context.currentPos();
					var self = TPath({pack: parts, name: name});
					var fields:Array<Field> = [for (v in e.variants) {
						name: v.name, pos: pos,
						kind: v.fields.length == 0 ? FVar(null) : FFun({
							args: [for (p in v.fields) {name: p.name, type: haxeType(p.ty, parts, [])}],
							ret: self, expr: null
						})
					}];
					Context.defineType({pack: parts, name: name, pos: pos,
						meta: [{name: ":keep", pos: pos}], kind: TDEnum, fields: fields});
				}
			}
			for (doc in described) {
				var f = {path: "", namespace: doc.lang, module: doc.module, pack: [doc.lang]};
				for (c in doc.classes) {
					define(f, doc.classes, c);
				}
			}
		}
	}

	/** The plugin libraries beside the program: `plugins/` under the
		directory the compiler writes the program to. */
	static function pluginLibraries():Array<String> {
		var output = haxe.macro.Compiler.getOutput();
		
		if (output == null || output == "") {
			return [];
		}
		var dir = haxe.io.Path.join([haxe.io.Path.directory(output), "plugins"]);
		Sys.println("looking for plugin libraries in " + dir);
		if (!FileSystem.isDirectory(dir)) {
			return [];
		}
		var extension = switch (Sys.systemName()) {
			case "Windows": "dll";
			case "Mac": "dylib";
			default: "so";
		}
		var libraries = [];
		for (entry in FileSystem.readDirectory(dir)) {
			if (haxe.io.Path.extension(entry) == extension) {
				libraries.push(haxe.io.Path.join([dir, entry]));
			}
		}
		libraries.sort(Reflect.compare);
		return libraries;
	}

	/** Where a described module of classpath `cp` lands: by its file's
		path under the root, the first directory the namespace and the
		rest the module; a file at the root is under its language's own
		namespace. */
	static function place(cp:String, doc:ModuleDesc):Found {
		var rel = haxe.io.Path.normalize(doc.path);
		var base = haxe.io.Path.normalize(cp);
		if (StringTools.startsWith(rel, base + "/")) {
			rel = rel.substr(base.length + 1);
		}
		var segments = haxe.io.Path.withoutExtension(rel).split("/");
		var stem = segments.pop();
		var namespace = segments.length > 0 ? segments[0] : doc.lang;
		var module = segments.length > 0 ? segments.slice(1).concat([stem]).join("/") : stem;
		return {
			path: doc.path,
			namespace: namespace,
			module: module,
			pack: segments.length > 0 ? segments.concat([stem]) : [doc.lang, stem]
		};
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
		same module that reports it, or one the module imports by a
		namespaced import, written `ns:module.Class`: the class emitted for
		that Wren module, else the Haxe class the import names.
		Plugin object identities are resolved across all plugin modules.
		Anything else is `Dynamic`. */
	static function haxeType(ty:Dynamic, pack:Array<String>, classes:Array<ClassDesc>):ComplexType {
		if (Std.isOfType(ty, String)) {
			return switch ((ty : String)) {
				case "Float": macro :Float;
				case "Int": macro :Int;
				case "Bool": macro :Bool;
				case "Str": macro :String;
				case "Buffer": macro :haxe.io.Bytes;
				case "Int64": macro :haxe.Int64;
				case "Void": macro :Void;
				default: macro :Dynamic;
			}
		}
		if (Reflect.hasField(ty, "Enum")) {
			var parts = (Reflect.field(ty, "Enum") : String).split(".");
			var name = parts.pop();
			return TPath({pack: parts, name: name});
		}
		if (Reflect.hasField(ty, "Object")) {
			var typeName:String = Reflect.field(ty, "Object");
			for (c in classes) {
				if (c.type_name == typeName) {
					return TPath({pack: pack, name: c.name});
				}
			}
			var pluginType = pluginTypes.get(typeName);
			if (pluginType != null) {
				return TPath(pluginType);
			}
			var colon = typeName.indexOf(":");
			var dot = typeName.lastIndexOf(".");
			if (colon > 0 && dot > colon) {
				var namespace = typeName.substr(0, colon);
				var module = typeName.substring(colon + 1, dot);
				var name = typeName.substr(dot + 1);
				for (f in modules) {
					if (f.namespace == namespace && f.module == module) {
						return TPath({pack: f.pack, name: name});
					}
				}
				// A Haxe class, under the spellings the registry resolves a
				// namespaced name through: the module as given, then under
				// the namespace. Found by its file, since typing a class
				// from inside this macro would type it before the classes
				// it imports exist.
				var dotted = module.split("/").join(".");
				for (candidate in [dotted, namespace + "." + dotted]) {
					var file = candidate.split(".").join("/") + ".hx";
					var found = try Context.resolvePath(file) catch (e:Dynamic) null;
					if (found != null) {
						var parts = candidate.split(".");
						var last = parts.pop();
						return TPath({pack: parts, name: last, sub: name == last ? null : name});
					}
				}
			}
			return macro :Dynamic;
		}
		if (Reflect.hasField(ty, "Array")) {
			// A foreign sequence behind its ref, or a Haxe array as it is.
			return macro :caribou.Sequence<Dynamic>;
		}
		if (Reflect.hasField(ty, "Function")) {
			// A function of a shape is a typed function value: Haxe calls it
			// as its own, and the bridge makes it one that takes the call
			// without boxing.
			var f = Reflect.field(ty, "Function");
			var params:Array<Dynamic> = Reflect.field(f, "params");
			var args = [for (p in params) haxeType(p, pack, classes)];
			return TFunction(args, haxeType(Reflect.field(f, "ret"), pack, classes));
		}
		return macro :Dynamic;
	}

	/** The class the emitted one extends: the emitted class of its
		superclass when that is a class of the same module, else
		`caribou.Ref`. A superclass from elsewhere, a Haxe class or one of
		another module, has no emitted class to extend. */
	static function parentOf(c:ClassDesc, classes:Array<ClassDesc>):Null<ClassDesc> {
		return c.superclass == null ? null : Lambda.find(classes, x -> x.name == c.superclass);
	}

	/** The constructor an instance of `c` is made by: its own, else the
		nearest superclass's of the same module, as Haxe inherits one. */
	static function constructorOf(c:ClassDesc, classes:Array<ClassDesc>):Null<MemberDesc> {
		var depth = 0;
		while (c != null && depth++ <= classes.length) {
			var own = Lambda.find(c.members, m -> m.kind == "constructor");
			if (own != null) {
				return own;
			}
			c = parentOf(c, classes);
		}
		return null;
	}

	/** The members `c` declares in Haxe. Instance members are its own,
		less those an emitted superclass declares: the superclass's wrapper
		reaches an override through the object's own dispatch. Statics are
		its own and its emitted superclasses', since Haxe does not inherit
		statics and Wren does; a static the class defines hides the
		superclass's of the same signature. */
	static function declared(c:ClassDesc, classes:Array<ClassDesc>):Array<MemberDesc> {
		var members = [];
		var seen = new Map<String, Bool>();
		for (m in c.members) {
			seen.set(m.kind + ":" + m.signature, true);
			members.push(m);
		}
		var parent = parentOf(c, classes);
		var depth = 0;
		while (parent != null && depth++ < classes.length) {
			for (m in parent.members) {
				var key = m.kind + ":" + m.signature;
				if (seen.exists(key)) {
					continue;
				}
				seen.set(key, true);
				if (m.kind == "static" || m.kind == "factory") {
					members.push(m);
				}
			}
			parent = parentOf(parent, classes);
		}
		// What the parent chain declares in Haxe is not declared again.
		var inherited = new Map<String, Bool>();
		parent = parentOf(c, classes);
		depth = 0;
		while (parent != null && depth++ < classes.length) {
			for (m in parent.members) {
				if (m.kind == "method" || m.kind == "getter" || m.kind == "setter") {
					inherited.set(m.kind + ":" + m.signature, true);
				}
			}
			parent = parentOf(parent, classes);
		}
		return members.filter(m -> !inherited.exists(m.kind + ":" + m.signature));
	}

	/** A placeholder of a type, for a `super()` whose constructor binds
		nothing: the runtime binds no object to an instance of a subclass
		that constructs itself. */
	static function placeholder(t:ComplexType):Expr {
		return switch (haxe.macro.ComplexTypeTools.toString(t)) {
			case "Float", "Int": macro 0;
			case "Bool": macro false;
			default: macro null;
		}
	}

	static function define(f:Found, classes:Array<ClassDesc>, c:ClassDesc):Void {
		var pos = Context.currentPos();
		var pack = f.pack;
		var self = TPath({pack: pack, name: c.name});
		var prefix = '${f.namespace}:${f.module}.${c.name}.';
		var fields:Array<Field> = [];
		var taken = new Map<String, Bool>();
		var properties = new Map<String, {get:Bool, set:Bool, type:ComplexType}>();
		var staticProperties = new Map<String, {get:Bool, set:Bool, type:ComplexType}>();
		var hasConstructor = false;
		var parent = parentOf(c, classes);
		var members = declared(c, classes);
		// The Haxe names the emitted superclasses take: a member of the
		// same name and another arity is spelled apart from them.
		var up = parent;
		var depth = 0;
		while (up != null && depth++ < classes.length) {
			for (m in up.members) {
				if (m.kind == "method" || m.kind == "getter" || m.kind == "setter") {
					taken.set(m.name, true);
				}
			}
			up = parentOf(up, classes);
		}

		function native(symbol:String, args:Array<FunctionArg>, ret:ComplexType, name:String):String {
			// The body is never run: genhl emits the native in its place.
			var body = switch (haxe.macro.ComplexTypeTools.toString(ret)) {
				case "Void": macro {};
				case "Float": macro return 0.0;
				case "Bool": macro return false;
				default: macro return null;
			}
			fields.push({
				name: name,
				pos: pos,
				access: [AStatic],
				meta: [{name: ":hlNative", params: [macro $v{LIB}, macro $v{symbol}], pos: pos}],
				kind: FFun({args: args, ret: ret, expr: body})
			});
			return name;
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

		for (m in members) {
			var arity = m.params.length;
			var callArgs = [for (p in m.params) macro $i{p.name}];
			var ret = haxeType(m.ret, pack, classes);
			// A number, a bool or a function comes back from the native in a
			// register, and nothing comes back from a `Null` result; anything
			// else as a boxed dynamic the wrapper casts.
			var nativeRet = switch (ret) {
				case TFunction(_, _): ret;
				default: switch (haxe.macro.ComplexTypeTools.toString(ret)) {
					case "Float", "Bool", "Void": ret;
					default: macro :Dynamic;
				}
			}
			var isVoid = haxe.macro.ComplexTypeTools.toString(ret) == "Void";
			// The wrapper's body: the native's result, or the call alone.
			function answer(call:Expr):Expr {
				return isVoid ? call : macro return $call;
			}
			// The native's Haxe name: distinct per kind, name and arity.
			var nativeName = "__" + m.kind + "_" + m.name + arity;
			switch (m.kind) {
				case "constructor" if (!hasConstructor):
					// `new Hud(3)`: the native makes the foreign object for
					// the fresh Haxe one and binds the two. The `super()` of
					// an emitted superclass takes placeholders: its native
					// binds nothing to an instance of a subclass that
					// constructs itself.
					hasConstructor = true;
					var init = native(prefix + "construct:" + m.signature, [{name: "self", type: self}].concat(typedArgs(m)), macro :Void, nativeName);
					var call = [macro this].concat(callArgs);
					var inherited = parent == null ? null : constructorOf(parent, classes);
					var supers = inherited == null ? [] : [for (p in inherited.params) placeholder(haxeType(p.ty, pack, classes))];
					fields.push({
						name: "new",
						pos: pos,
						access: [APublic],
						kind: FFun({args: typedArgs(m), ret: null, expr: macro {
							super($a{supers});
							$i{init}($a{call});
						}})
					});
				case "factory", "static":
					var isSetter = m.signature.indexOf("=(") >= 0;
					var isGetter = m.signature.indexOf("(") < 0;
					if (isSetter) {
						// A static setter: the property's `set`.
						var valueType = typedArgs(m)[0].type;
						var target = native(prefix + "static:" + m.signature, [{name: "value", type: valueType}], macro :Void, nativeName);
						var p = staticProperties.get(m.name);
						if (p == null) {
							staticProperties.set(m.name, p = {get: false, set: false, type: valueType});
						}
						p.set = true;
						fields.push({
							name: "set_" + m.name,
							pos: pos,
							access: [AStatic, AInline],
							kind: FFun({args: [{name: "value", type: valueType}], ret: valueType, expr: macro {
								$i{target}(value);
								return value;
							}})
						});
					} else if (isGetter) {
						// A static getter: the property's `get`.
						var target = native(prefix + "static:" + m.signature, [], nativeRet, nativeName);
						var p = staticProperties.get(m.name);
						if (p == null) {
							staticProperties.set(m.name, p = {get: false, set: false, type: ret});
						}
						p.get = true;
						p.type = ret;
						fields.push({
							name: "get_" + m.name,
							pos: pos,
							access: [AStatic, AInline],
							kind: FFun({args: [], ret: ret, expr: answer(macro $i{target}())})
						});
					} else {
						var target = native(prefix + "static:" + m.signature, typedArgs(m), nativeRet, nativeName);
						var name = unique(m.name, arity);
						fields.push({
							name: name,
							pos: pos,
							access: [APublic, AStatic, AInline],
							kind: FFun({args: typedArgs(m), ret: ret, expr: answer(macro $i{target}($a{callArgs}))})
						});
					}
				case "method":
					var target = native(prefix + m.signature, [{name: "self", type: self}].concat(typedArgs(m)), nativeRet, nativeName);
					var name = unique(m.name, arity);
					var call = [macro this].concat(callArgs);
					fields.push({
						name: name,
						pos: pos,
						access: [APublic, AInline],
						kind: FFun({args: typedArgs(m), ret: ret, expr: answer(macro $i{target}($a{call}))})
					});
				case "getter":
					var target = native(prefix + m.signature, [{name: "self", type: self}], nativeRet, nativeName);
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
						kind: FFun({args: [], ret: ret, expr: answer(macro $i{target}(this))})
					});
				case "setter":
					var valueType = typedArgs(m)[0].type;
					var target = native(prefix + m.signature, [{name: "self", type: self}, {name: "value", type: valueType}], macro :Void, nativeName);
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
		for (name => p in staticProperties) {
			taken.set(name, true);
			fields.push({
				name: name,
				pos: pos,
				access: [APublic, AStatic],
				kind: FProp(p.get ? "get" : "never", p.set ? "set" : "never", p.type)
			});
		}
		// A class without a constructor of its own inherits its emitted
		// superclass's, as in Wren; under `caribou.Ref` it has one that
		// binds nothing, for an instance the runtime binds.
		if (!hasConstructor && parent == null) {
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
			kind: TDClass(parent == null ? {pack: ["caribou"], name: "Ref"} : {pack: pack, name: parent.name}),
			fields: fields
		});
	}
	#end
}
