package caribou.hxsl;

#if macro
import haxe.macro.Context;
import haxe.macro.Expr;

class Build {
	/** The source of the shader class `path`, for `@:import` and `@:extends`. **/
	static function source(path:String):Ast.Expr {
		switch (Context.follow(Context.getType(path))) {
			case TInst(c, _):
				for (m in c.get().meta.get())
					if (m.name == ":src")
						return new MacroParser().parseExpr(m.params[0]);
			default:
		}
		throw '$path is not an HXSL shader';
	}

	public static function shader():Array<Field> {
		var fields = Context.getBuildFields();
		var src = Lambda.find(fields, f -> f.name == "SRC");
		if (src == null)
			Context.error("an HXSL shader declares static var SRC", Context.currentPos());
		var expr = switch (src.kind) {
			case FVar(_, e) if (e != null): e;
			default: Context.error("SRC is the shader's source", src.pos);
		}
		var local = Context.getLocalClass().get();
		local.meta.add(":src", [expr], expr.pos);
		fields.remove(src);
		try {
			var compiled = Compiler.compile(local.name, expr, source, (msg, pos) -> Context.warning(msg, pos));
			// Only helpers: a module other shaders import, with nothing to print.
			if (compiled == null)
				return fields;
			var wgsl = compiled.wgsl;
			var pos = expr.pos;
			function constant(name:String, value:Expr, doc:String)
				fields.push({
					name: name,
					doc: doc,
					access: [APublic, AStatic, AInline],
					kind: FVar(null, value),
					pos: pos
				});
			constant("WGSL", macro $v{wgsl}, "The shader as WGSL, for `device.createShader`.");
			var l = compiled.layout;
			constant("PARAMS_SIZE", macro $v{l.paramsSize}, "Bytes of the params uniform buffer at group 0, binding 0.");
			for (p in l.params)
				constant('PARAM_${p.name}', macro $v{p.offset}, 'Byte offset of `${p.name}` in the params buffer.');
			for (t in l.textures)
				constant('TEXTURE_${t.name}', macro $v{t.binding}, 'Binding of `${t.name}`; its sampler is the next binding.');
			for (b in l.buffers)
				constant('BUFFER_${b.name}', macro $v{b.binding}, 'Binding of `${b.name}`.');
			for (i in l.inputs)
				constant('INPUT_${i.name}', macro $v{i.location}, 'Vertex attribute location of `${i.name}`.');
			for (t in l.targets)
				constant('TARGET_${t.name}', macro $v{t.location}, 'Color target of `${t.name}`.');
			for (o in l.overrides)
				constant('CONST_${o.name}', macro $v{o.key}, 'Pipeline constant key of `@const ${o.name}`.');
		} catch (e:Ast.Error) {
			Context.error(e.msg, e.pos);
		}
		return fields;
	}
}
#end
