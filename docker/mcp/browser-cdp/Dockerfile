FROM mcr.microsoft.com/playwright/python:v1.52.0-noble

WORKDIR /app

RUN pip install --no-cache-dir --break-system-packages \
    fastapi==0.115.12 \
    uvicorn==0.34.2 \
    playwright==1.52.0

COPY app.py .

EXPOSE 9030

HEALTHCHECK --interval=5s --start-period=5s --timeout=5s CMD curl -f http://localhost:9030/health || exit 1

CMD ["python", "-m", "uvicorn", "app:app", "--host", "0.0.0.0", "--port", "9030"]
