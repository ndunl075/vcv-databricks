#!/usr/bin/env bash
# Prints the blob id of the mutator source (part of the dataset cache key). Run from the checkout.
cd "$(dirname "$0")/.." && git rev-parse origin/bench:bench/ood/mutate.py
