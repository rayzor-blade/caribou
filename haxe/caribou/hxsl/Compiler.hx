package caribou.hxsl;

import caribou.hxsl.Ast;
import caribou.hxsl.WgslOut.WgslLayout;

using caribou.hxsl.Ast;

/**
	HXSL to WGSL, the way Heaps compiles a shader, but at compile time: check
	it, inline its helpers, link it with its outputs, split the stages, drop
	dead code and print WGSL.
**/
class Compiler {
	/**
		`source` is the HXSL block of the shader `name`; `load` finds the block
		of a shader it imports or extends. Null for a module of helpers alone,
		which has nothing to print.
	**/
	public static function compile(name:String, source:haxe.macro.Expr, load:String->Expr,
			warning:(String, Position) -> Void):Null<{wgsl:String, layout:WgslLayout}> {
		var checker = new Checker();
		checker.warning = warning;
		checker.loadShader = load;
		var data = checker.check(name, new MacroParser().parseExpr(source));
		if (!Lambda.exists(data.funs, f -> f.kind == Vertex || f.kind == Fragment || f.kind == Main))
			return null;
		var eval = new Eval();
		eval.inlineCalls = true;
		var evaluated = eval.eval(data);
		var compute = Lambda.exists(data.funs, f -> f.kind == Main);
		var shaders = compute ? [evaluated] : [evaluated, outputs(evaluated, source.pos)];
		var linker = new Linker(compute ? Compute : Default);
		var linked = linker.link(shaders);
		var splitter = new Splitter();
		var stages = new Dce().dce(splitter.split(linked, false));
		// Linking drops a struct member's parent; its path stays with the linker.
		var paths = new Map<Int, String>();
		for (a in linker.allVars) {
			paths.set(a.v.id, a.path);
			var split = @:privateAccess splitter.varMap.get(a.v);
			if (split != null)
				paths.set(split.id, a.path);
		}
		var out = new WgslOut(data.vars, paths);
		return {wgsl: out.run(stages, compute), layout: out.layout};
	}

	/**
		What the stages produce, as Heaps' cache declares it for a driver:
		`output.position` from the vertex stage, and every other field of
		the shader's `output` from the fragment stage, in declaration order.
	**/
	static function outputs(shader:ShaderData, pos:Position):ShaderData {
		var declared = Lambda.find(shader.vars, v -> v.name == "output");
		var fields = switch (declared == null ? null : declared.type) {
			case TStruct(vl): vl;
			default: throw new Error("a render shader declares var output : { position : Vec4, color : Vec4 }", pos);
		}
		if (!Lambda.exists(fields, f -> f.name == "position"))
			throw new Error("output has no position for the vertex stage", pos);
		var output:TVar = {
			id: Tools.allocVarId(),
			name: "output",
			type: TStruct([]),
			kind: Var
		};
		var link:ShaderData = {name: "output", vars: [output], funs: []};
		function assign(field:TVar):TExpr {
			var read:TVar = {
				id: Tools.allocVarId(),
				name: field.name,
				type: field.type,
				kind: Var,
				parent: output
			};
			switch (output.type) {
				case TStruct(vl): vl.push(read);
				default:
			}
			// Named, so the field's name survives the linker renaming a clash with a param.
			var out:TVar = {
				id: Tools.allocVarId(),
				name: field.name,
				type: field.type,
				kind: Output,
				qualifiers: [Name(field.name)]
			};
			link.vars.push(out);
			return {e: TBinop(OpAssign, {e: TVar(out), t: out.type, p: pos}, {e: TVar(read), t: read.type, p: pos}), t: TVoid, p: pos};
		}
		function stage(kind:FunctionKind, body:Array<TExpr>) {
			link.funs.push({
				kind: kind,
				ref: {id: Tools.allocVarId(), name: ("" + kind).toLowerCase(), type: TFun([]), kind: Function},
				args: [],
				ret: TVoid,
				expr: {e: TBlock(body), t: TVoid, p: pos}
			});
		}
		stage(Vertex, [for (f in fields) if (f.name == "position") assign(f)]);
		stage(Fragment, [for (f in fields) if (f.name != "position") assign(f)]);
		return link;
	}

}
