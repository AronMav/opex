import time

import app


def _reset_session_state():
    app.sessions.clear()
    app.session_last_used.clear()
    app.persistent_sessions.clear()


def test_ephemeral_session_idle_past_ttl_is_expired():
    _reset_session_state()
    sid = "ephemeral1"
    app.session_last_used[sid] = time.time() - (app.SESSION_TTL + 10)
    try:
        expired = app._expired_sids(time.time())
        assert sid in expired
    finally:
        _reset_session_state()


def test_profile_session_idle_past_ttl_is_not_expired():
    _reset_session_state()
    sid = "profile1"
    app.session_last_used[sid] = time.time() - (app.SESSION_TTL + 10)
    app.persistent_sessions.add(sid)
    try:
        expired = app._expired_sids(time.time())
        assert sid not in expired
    finally:
        _reset_session_state()
