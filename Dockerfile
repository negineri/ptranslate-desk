# サーバー (server/translator.py) と Web の画面 (server/static/)。Python の標準ライブラリだけで動く。
# 前段に認証プロキシを置き、X-Forwarded-User を付けて渡すこと (README.md)
FROM python:3.13-slim
RUN useradd --system --uid 10001 --no-create-home translator
WORKDIR /app
COPY server/translator.py /app/translator.py
COPY server/static/ /app/static/
ENV TR_LISTEN_HOST=0.0.0.0 \
    TR_LISTEN_PORT=8002 \
    TR_STATIC_DIR=/app/static \
    PYTHONDONTWRITEBYTECODE=1
USER 10001
EXPOSE 8002
CMD ["python3", "-I", "/app/translator.py"]
