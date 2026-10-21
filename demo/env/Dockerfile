# VHS (ttyd, ffmpeg, Chromium) with tmux, git, jq and gifsicle: demo/render.sh renders the tapes with it.
FROM ghcr.io/charmbracelet/vhs:v0.12.1
RUN apt-get update \
  && apt-get install -y --no-install-recommends gifsicle git jq tmux \
  && rm -rf /var/lib/apt/lists/*
