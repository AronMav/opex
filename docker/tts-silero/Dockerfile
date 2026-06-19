FROM python:3.12-slim

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        git ffmpeg libsndfile1 \
    && rm -rf /var/lib/apt/lists/*

# CPU-torch (Silero и RUAccent на CPU — лёгкие, GPU не нужен).
RUN pip install --no-cache-dir torch --index-url https://download.pytorch.org/whl/cpu

COPY requirements.txt /app/requirements.txt
RUN pip install --no-cache-dir -r /app/requirements.txt

# espeak-ng: English->Cyrillic G2P used by normalize.transliterate_latin.
# Separate layer (after pip) so the heavy torch layer stays cached.
RUN apt-get update && apt-get install -y --no-install-recommends espeak-ng \
    && rm -rf /var/lib/apt/lists/*

COPY normalize.py server.py /app/
WORKDIR /app
EXPOSE 8000
CMD ["python", "-m", "uvicorn", "server:app", "--host", "0.0.0.0", "--port", "8000"]
