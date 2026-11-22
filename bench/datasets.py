"""Names, seeds and sizes of the generated base datasets (see gen.sh)."""
BASES = {  # name: (seed, events, clicks)
    "dev": (1, 1_000_000, 250_000),
    "p20a": (20, 20_000_000, 5_000_000),
    "p20b": (21, 20_000_000, 5_000_000),
    "f60": (60, 60_000_000, 15_000_000),
}
SIZE_BASE = {"1m": "dev", "20m": "p20a", "60m": "f60"}
