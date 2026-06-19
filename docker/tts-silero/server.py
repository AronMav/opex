import io
import os
import re
import subprocess
import threading

import numpy as np
import soundfile as sf
import torch
from fastapi import FastAPI
from fastapi.responses import JSONResponse, Response
from pydantic import BaseModel

from normalize import normalize

MODEL_ID = os.environ.get("SILERO_MODEL_ID", "v5_1_ru")
LANGUAGE = os.environ.get("SILERO_LANGUAGE", "ru")
DEVICE = os.environ.get("SILERO_DEVICE", "cpu")
DEFAULT_SPEAKER = os.environ.get("SILERO_DEFAULT_SPEAKER", "kseniya")
SAMPLE_RATE = int(os.environ.get("SILERO_SAMPLE_RATE", "48000"))
HUB_DIR = os.environ.get("SILERO_CACHE", "/models")
# RUAccent по умолчанию ВЫКЛЮЧЕН: текущая версия пакета падает на инференсе
# (onnxruntime требует token_type_ids). Включить: SILERO_USE_ACCENT=true.
USE_ACCENT = os.environ.get("SILERO_USE_ACCENT", "false").lower() == "true"
SPEAKERS = {"aidar", "baya", "kseniya", "xenia", "eugene"}
_VOICE_MAP = {"alloy": "eugene", "echo": "aidar", "fable": "baya",
              "onyx": "eugene", "nova": "kseniya", "shimmer": "xenia"}

app = FastAPI()
_model = None
_accent = None
_LOCK = threading.Lock()  # apply_tts не потокобезопасен — синтез сериализуем

# NNPACK на этом CPU не поддерживается и сыплет предупреждениями в лог — отключаем (косметика).
try:
    torch.backends.nnpack.enabled = False
except Exception:
    pass


def _get():
    global _model, _accent
    if _model is None:
        torch.hub.set_dir(HUB_DIR)
        os.makedirs(HUB_DIR, exist_ok=True)
        with open(os.path.join(HUB_DIR, "trusted_list"), "w") as f:
            f.write("snakers4_silero-models\n")
        torch.set_num_threads(int(os.environ.get("SILERO_THREADS", "4")))
        _model, _ = torch.hub.load(
            "snakers4/silero-models", "silero_tts",
            language=LANGUAGE, speaker=MODEL_ID, trust_repo=True,
        )
        _model.to(torch.device(DEVICE))
        # RUAccent грузим только если включён флагом (по умолчанию off — пакет сломан
        # на инференсе). use_dictionary=False — нейро-режим. Любой сбой → без ударений.
        if USE_ACCENT:
            try:
                from ruaccent import RUAccent
                a = RUAccent()
                a.load(omograph_model_size="turbo3.1", use_dictionary=False,
                       workdir=os.path.join(HUB_DIR, "ruaccent"))
                _accent = a
            except Exception as e:
                print(f"[silero-tts] RUAccent не загрузился ({e!r}); синтез без ударений")
                _accent = None
    return _model, _accent


def _by_words(sent, limit):
    """Слишком длинное предложение режем по словам, чтобы не превысить лимит apply_tts."""
    if len(sent) <= limit:
        return [sent]
    out, cur = [], ""
    for w in sent.split():
        if len(cur) + len(w) + 1 > limit and cur:
            out.append(cur); cur = w
        else:
            cur = (cur + " " + w).strip()
    if cur:
        out.append(cur)
    return out


def _split(text, limit=900):
    parts, cur = [], ""
    for sent in re.split(r"(?<=[.!?])\s+", text):
        for piece in _by_words(sent, limit):
            if len(cur) + len(piece) + 1 > limit and cur:
                parts.append(cur.strip()); cur = piece
            else:
                cur = (cur + " " + piece).strip()
    if cur:
        parts.append(cur)
    return parts or [text]


def _tts(model, speaker, text):
    wav = model.apply_tts(text=text, speaker=speaker, sample_rate=SAMPLE_RATE,
                          put_accent=True, put_yo=True)
    return wav.numpy() if hasattr(wav, "numpy") else np.asarray(wav)


def _synthesize(text, speaker):
    global _accent
    norm = normalize(text)
    if not norm.strip():
        norm = "Пусто"
    # сериализуем загрузку модели и синтез: apply_tts на общей модели не потокобезопасен,
    # параллельные запросы Open WebUI иначе обрывают/портят аудио друг друга.
    with _LOCK:
        model, accent = _get()
        audio = []
        for chunk in _split(norm):
            if not chunk.strip():
                continue
            accented = chunk
            if accent is not None:
                try:
                    accented = accent.process_all(chunk)
                except Exception as e:  # рантайм-сбой RUAccent — отключаем, без ударений
                    print(f"[silero-tts] RUAccent сбой на инференсе ({e!r}); дальше без ударений")
                    _accent = None
                    accent = None
                    accented = chunk
            audio.append(_tts(model, speaker, accented))
        if not audio:  # озвучивать нечего (текст из одной пунктуации) — заглушка
            audio.append(_tts(model, speaker, "Пусто"))
    return np.concatenate(audio) if len(audio) > 1 else audio[0]


def _to_format(wav, fmt):
    buf = io.BytesIO()
    sf.write(buf, wav, SAMPLE_RATE, format="WAV")
    data = buf.getvalue()
    fmt = (fmt or "mp3").lower()
    if fmt == "wav":
        return data, "audio/wav"
    codec = {"mp3": ("mp3", "audio/mpeg"), "opus": ("opus", "audio/ogg"),
             "aac": ("adts", "audio/aac"), "flac": ("flac", "audio/flac")}.get(fmt, ("mp3", "audio/mpeg"))
    out = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error",
                          "-i", "pipe:0", "-f", codec[0], "pipe:1"],
                         input=data, stdout=subprocess.PIPE, check=True)
    return out.stdout, codec[1]


class SpeechRequest(BaseModel):
    model: str | None = None
    input: str
    voice: str | None = None
    response_format: str | None = "mp3"
    speed: float | None = 1.0


@app.get("/health")
def health():
    return {"status": "ok", "model": MODEL_ID, "speaker": DEFAULT_SPEAKER}


@app.post("/v1/audio/speech")
def speech(req: SpeechRequest):
    if not req.input or not req.input.strip():
        return JSONResponse(status_code=400, content={"error": "empty input"})
    voice = (req.voice or DEFAULT_SPEAKER)
    speaker = voice if voice in SPEAKERS else _VOICE_MAP.get(voice, DEFAULT_SPEAKER)
    wav = _synthesize(req.input, speaker)
    data, media = _to_format(wav, req.response_format)
    return Response(content=data, media_type=media)
