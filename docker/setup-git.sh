#!/bin/bash
# Configure git credentials from environment variables injected by Opex engine.
# Supports any git-capable OAuth provider: GITHUB, GITLAB, BITBUCKET, etc.
# Engine sets {PROVIDER}_GIT_TOKEN and {PROVIDER}_GIT_HOST for each connected binding.

set -e

# Configure git credentials for all git-capable OAuth providers
for provider in GITHUB GITLAB BITBUCKET; do
  token_var="${provider}_GIT_TOKEN"
  host_var="${provider}_GIT_HOST"
  if [ -n "${!token_var}" ]; then
    git config --global credential.helper store
    echo "https://x-access-token:${!token_var}@${!host_var}" >> "$HOME/.git-credentials"
  fi
done

# Secure credentials file
[ -f "$HOME/.git-credentials" ] && chmod 600 "$HOME/.git-credentials"

# Git identity (set by engine from OAuth userinfo)
[ -n "$GIT_AUTHOR_NAME" ] && git config --global user.name "$GIT_AUTHOR_NAME"
[ -n "$GIT_AUTHOR_EMAIL" ] && git config --global user.email "$GIT_AUTHOR_EMAIL"

# Keep container alive (sandbox.rs sends commands via docker exec)
exec tail -f /dev/null
