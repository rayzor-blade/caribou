# The heap test: every round makes an object and a list nothing keeps.


class Item:
    def __init__(self, n: int):
        self.n = n
        self.tags = [n, n + 1, n + 2]


def churn(rounds: int) -> int:
    total = 0
    for i in range(rounds):
        item = Item(i)
        total += item.tags[1]
    return total
