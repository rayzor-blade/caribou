# A behaviour a Haxe engine keeps one of per entity and steps.


class Mover:
    def __init__(self, speed: float):
        self.speed = speed
        self.x = 0.0

    def step(self, dt: float) -> float:
        self.x = self.x + self.speed * dt
        return self.x
