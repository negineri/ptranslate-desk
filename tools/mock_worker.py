#!/usr/bin/env python3
"""ローカル試験専用のワーカー。翻訳エンジンや外部サービスへは接続しない。"""

import http.client
import json
import urllib.parse


class MockWorker:
    def __init__(self, server_url, country="JP", network="192.0.2.1", *, tls_context=None):
        url = urllib.parse.urlsplit(server_url)
        scheme = "https" if tls_context is not None else "http"
        if url.scheme != scheme or url.hostname not in ("127.0.0.1", "localhost", "::1"):
            raise ValueError("モックはローカルのサーバーだけに接続できます")
        self.host, self.port = url.hostname, url.port
        self.tls_context = tls_context
        self.country, self.network = country, network
        self.secret = ""
        self.timeout = 5

    def call(self, path, body=None, owner=None, method="POST", secret=None):
        headers = {
            "Content-Type": "application/json",
            "X-Test-Country": self.country,
            "X-Test-IP": self.network,
        }
        if owner:
            headers["X-Forwarded-User"] = owner
        if secret is not None or self.secret:
            headers["Authorization"] = "Bearer " + (self.secret if secret is None else secret)
        if self.tls_context is None:
            conn = http.client.HTTPConnection(self.host, self.port, timeout=self.timeout)
        else:
            conn = http.client.HTTPSConnection(
                self.host, self.port, timeout=self.timeout, context=self.tls_context
            )
        try:
            conn.request(method, path, json.dumps(body) if body is not None else None, headers)
            response = conn.getresponse()
            data = response.read()
            return response.status, json.loads(data) if data else None
        finally:
            conn.close()

    def register(self, owner):
        status, data = self.call("/api/worker/register", {"terms_version": 1}, owner)
        if status == 200:
            self.secret = data["secret"]
        return status, data

    def poll(self, model, version="0.5.0"):
        return self.call("/api/worker/poll", {"model": model, "agent_version": version})

    def result(self, assignment_id, seq, **fields):
        return self.call(
            "/api/worker/result", {"assignment_id": assignment_id, "seq": seq, **fields}
        )
