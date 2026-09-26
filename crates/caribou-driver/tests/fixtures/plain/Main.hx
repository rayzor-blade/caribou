class Point {
  public var x:Int;
  public function new(x:Int) { this.x = x; }
}

class Main {
  static function main() {
    var points = [for (i in 0...1000) new Point(i)];
    var total = 0;
    for (p in points) total += p.x;
    Sys.println('total $total');
  }
}
