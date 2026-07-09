"""SMTP primitive — stateless email send.

Credentials are passed in the request body. No environment reads.
"""

import logging
import smtplib
from email.mime.multipart import MIMEMultipart
from email.mime.text import MIMEText
from typing import Optional

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel

log = logging.getLogger("toolgate.primitives.smtp")
router = APIRouter(prefix="/primitives/smtp", tags=["primitives"])


class SmtpSendRequest(BaseModel):
    server: str
    port: int = 587
    user: str
    password: str
    to: str
    subject: str
    body: str
    html: bool = False
    reply_to: Optional[str] = None
    use_tls: bool = True


@router.post("/send")
def send(req: SmtpSendRequest):
    # F029: plain `def` → FastAPI threadpool; blocking smtplib on the async loop
    # froze all of single-process toolgate for the hang duration.
    """Send an email via SMTP with optional STARTTLS. Stateless — all creds in body."""
    if req.html:
        msg = MIMEMultipart("alternative")
        msg.attach(MIMEText(req.body, "html", "utf-8"))
    else:
        msg = MIMEText(req.body, "plain", "utf-8")

    msg["From"] = req.user
    msg["To"] = req.to
    msg["Subject"] = req.subject
    if req.reply_to:
        msg["Reply-To"] = req.reply_to

    try:
        with smtplib.SMTP(req.server, req.port, timeout=15) as smtp:
            smtp.ehlo()
            if req.use_tls:
                smtp.starttls()
                smtp.ehlo()
            smtp.login(req.user, req.password)
            smtp.sendmail(req.user, [req.to], msg.as_string())
    except smtplib.SMTPAuthenticationError as e:
        raise HTTPException(401, f"SMTP auth failed: {e}") from e
    except smtplib.SMTPException as e:
        raise HTTPException(502, f"SMTP error: {e}") from e
    except OSError as e:
        raise HTTPException(502, f"SMTP connection failed: {e}") from e

    return {"status": "sent", "to": req.to, "subject": req.subject}
