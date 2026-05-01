FROM debian:bookworm-slim

WORKDIR /app
COPY --chown=10001:10001 app/start.sh /app/start.sh

RUN groupadd --system --gid 10001 altair \
  && useradd --system --uid 10001 --gid altair --home-dir /nonexistent --shell /usr/sbin/nologin altair \
  && chmod +x /app/start.sh

USER 10001

CMD ["/app/start.sh"]
