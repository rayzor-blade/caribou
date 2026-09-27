// Colours for the flock: a hue in [0, 1) that drifts with time.
class Palette {
  static hue(index, time) { (index * 0.618034 + time * 0.05) % 1 }
}
