FROM mcr.microsoft.com/playwright/python:v1.52.0-noble

WORKDIR /app

COPY requirements.txt .
RUN pip install --no-cache-dir --break-system-packages -r requirements.txt

COPY app.py .
COPY automation_actions.py .
COPY ssrf_guard.py .
COPY ssrf_proxy.py .
COPY profiles.py .
COPY stealth.py .

EXPOSE 9020

CMD ["python", "-m", "uvicorn", "app:app", "--host", "0.0.0.0", "--port", "9020"]
