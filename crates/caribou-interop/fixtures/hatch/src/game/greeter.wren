import "@hatch:greet" for Greet

class Greeter {
  static greet(name) { Greet.hello(name) + "!" }
}
