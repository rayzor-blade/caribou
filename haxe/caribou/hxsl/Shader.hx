package caribou.hxsl;

/**
	A shader written in HXSL. A class that implements this declares its
	source in `static var SRC = { ... }`; the build macro checks it when
	the program compiles and replaces it with `static inline var WGSL`, the
	source the gpu plugin compiles.
**/
@:autoBuild(caribou.hxsl.Build.shader())
interface Shader {}
