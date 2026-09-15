// A module a sibling imports plainly (`import "format"` from
// game/hud.wren), so it loads from the roots beside its importer, or
// compiled from a bundle as its importer is.
class Format {
  static score(n) { "score %(n)" }
}
