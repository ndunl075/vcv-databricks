# Tooling image for the benchmark harness (oracle, compare, mutator). Native arch; never used to time entries.
FROM python:3.13-slim
RUN pip install --no-cache-dir duckdb==1.3.2 polars==1.31.0 pyarrow==20.0.0 numpy==2.3.2
ENV HOME=/tmp PYTHONDONTWRITEBYTECODE=1
