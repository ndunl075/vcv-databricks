# Tooling image for the benchmark harness (oracle, compare, mutator). Native arch; never used to time entries.
FROM python:3.13-slim
ENV PIP_DEFAULT_TIMEOUT=60 PIP_RETRIES=10 PIP_NO_CACHE_DIR=1
RUN pip install duckdb==1.3.2
RUN pip install pyarrow==20.0.0
ENV HOME=/tmp PYTHONDONTWRITEBYTECODE=1
