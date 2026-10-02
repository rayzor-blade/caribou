# Python objects, closures and errors another language reaches.


class Account:
    def __init__(self, owner: str, balance: float):
        self.owner = owner
        self.balance = balance

    def deposit(self, amount: float) -> float:
        if amount < 0.0:
            raise ValueError("a deposit cannot be negative")
        self.balance += amount
        return self.balance

    def same(self):
        return self

    @staticmethod
    def scaler(k: float):
        return lambda x: x * k


def open_account(owner: str) -> Account:
    return Account(owner, 0.0)
