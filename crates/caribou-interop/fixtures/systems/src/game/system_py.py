# A Python system: counts its steps in byte 0, and writes the step's time as
# an f32 at byte 4.
from game.Systems import Systems


def step(column, dt: float):
    column.setU8(0, column.getU8(0) + 1)
    column.setF32(4, dt)


def install():
    Systems.register(step)


def fail(column, dt: float):
    raise ValueError("python failed")


def installFailing():
    Systems.registerFailing(fail)


def fail_dynamic(column, dt: float):
    raise ValueError("python dynamic failed")


def installFailingDynamic():
    Systems.registerFailingDynamic(fail_dynamic)
