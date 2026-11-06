#!/usr/bin/env python3
"""Prueba la ventana Tauri real con WebDriver y una configuración temporal."""

import argparse
import base64
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import traceback
from http.client import HTTPConnection, HTTPException, RemoteDisconnected
from urllib.parse import urlsplit


class Window:
    def __init__(self, endpoint):
        url = urlsplit(endpoint)
        if url.scheme != "http" or url.hostname not in {"127.0.0.1", "localhost"}:
            raise ValueError("WebDriver debe ejecutarse en el equipo local")
        self.endpoint = endpoint
        self.session = None
        self.host, self.port = url.hostname, url.port or 80
        self.http = HTTPConnection(self.host, self.port, timeout=60)

    def close(self):
        self.http.close()

    def command(self, method, path, body=None, *, read_only=False):
        data = None if body is None else json.dumps(body).encode()
        attempts = 3 if read_only or method == "GET" else 1
        for attempt in range(attempts):
            try:
                # No propagar Connection: close al proxy de tauri-driver.
                self.http.request(method, path, body=data,
                                  headers={"Content-Type": "application/json"})
                response = self.http.getresponse()
                payload = response.read()
                if response.status >= 400:
                    raise RuntimeError(payload.decode(errors="replace"))
                # Un error devuelto por el JavaScript es un dato, no un error HTTP.
                return json.loads(payload).get("value")
            except (OSError, HTTPException) as error:
                self.http.close()
                self.http = HTTPConnection(self.host, self.port, timeout=60)
                transient = isinstance(error, (RemoteDisconnected, ConnectionResetError))
                if not transient or attempt + 1 == attempts:
                    raise
                print(f"Reintento de lectura WebDriver {attempt + 2}/{attempts}: {error}", flush=True)
                time.sleep(0.15 * (attempt + 1))

    def call(self, method, path, body=None, *, read_only=False):
        return self.command(method, f"/session/{self.session}{path}", body,
                            read_only=read_only)

    def read_dom(self, script, *args):
        """Solo consultas del DOM, sin efectos sobre la aplicación."""
        return self.call("POST", "/execute/sync", {"script": script, "args": list(args)},
                         read_only=True)

    def promise(self, expression, *args, read_only=False):
        script = """
        const done = arguments[arguments.length - 1];
        Promise.resolve().then(async () => {
        """ + expression + """
        }).then(value => done({ok: true, value}),
                error => done({ok: false, error: String(error)}));
        """
        return self.call("POST", "/execute/async", {"script": script, "args": list(args)},
                         read_only=read_only)

    def invoke(self, command, args=None, *, read_only=False):
        result = self.promise("return await window.__TAURI__.core.invoke(arguments[0], arguments[1]);",
                              command, args or {}, read_only=read_only or command in {"session", "recent"})
        assert result["ok"], result
        return result.get("value")

    def wait(self, predicate, description, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.2)
        raise AssertionError(f"Tiempo agotado: {description}")

    def exists(self, selector):
        return self.read_dom("return document.querySelector(arguments[0]) !== null;", selector)

    def text(self, selector):
        return self.read_dom("return document.querySelector(arguments[0])?.textContent ?? '';", selector)

    def element(self, selector):
        result = self.call("POST", "/element", {"using": "css selector", "value": selector},
                           read_only=True)
        return result["element-6066-11e4-a52e-4f735466cecf"]

    def click(self, selector):
        self.call("POST", f"/element/{self.element(selector)}/click", {})

    def type(self, selector, text):
        self.call("POST", f"/element/{self.element(selector)}/value", {"text": text})

    def open_file(self, path):
        result = self.promise("await window.__TAURI__.event.emit('open-path', arguments[0]);", str(path))
        assert result["ok"], result
        self.wait(lambda: path.name in self.text(".server-pill")
                  and self.exists('.app[aria-busy="false"]'), f"abrir {path.name}")

    def read_query(self, query):
        session = self.invoke("session")
        reply = self.invoke("request", {
            "method": "POST", "url": session["url"] + "/v1/default/query",
            "headers": {"content-type": "application/json"},
            "body": json.dumps({"query": query}),
        }, read_only=True)
        assert reply["status"] == 200, reply
        return json.loads(reply["body"])["rows"]

    def capture(self, directory, name):
        # Esperar fuentes, animaciones de entrada y dos repintados. El DOM
        # puede estar actualizado antes de que WebKit componga la imagen.
        result = self.promise("""
            await document.fonts.ready;
            await Promise.all(document.getAnimations()
                .filter(animation => Number.isFinite(animation.effect?.getComputedTiming().iterations))
                .map(animation => animation.finished.catch(() => {})));
            await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
        """, read_only=True)
        assert result["ok"], result
        png = self.call("GET", "/screenshot")
        (directory / f"{name}.png").write_bytes(base64.b64decode(png, validate=True))


def free_ports():
    sockets = [socket.socket(), socket.socket()]
    try:
        for sock in sockets:
            sock.bind(("127.0.0.1", 0))
        return [sock.getsockname()[1] for sock in sockets]
    finally:
        for sock in sockets:
            sock.close()


def scenario(window, files, artifacts):
    checks = []
    window.wait(lambda: window.exists(".start-card"), "pantalla inicial")
    window.capture(artifacts, "01-inicio")
    checks.append("La ventana muestra la pantalla inicial.")

    first = files / "memoria de prueba.redb"
    second = files / "otra base.redb"
    invalid = files / "archivo invalido.redb"
    invalid.write_text("Este archivo no es una base redb.")
    # El evento es el mismo que utiliza el menú de archivos recientes.
    # Los selectores nativos de archivos no se simulan ni se dan por probados.
    window.open_file(first)
    window.click(".topbar .ns-menu > button.primary")
    window.click(".menu.right .menu-item:first-child")
    window.wait(lambda: window.exists(".modal"), "formulario de nodo")
    window.type(".modal .label-input input", "Person")
    window.click('.modal input[placeholder="Ana García"]')
    window.type('.modal input[placeholder="Ana García"]', "Ana de prueba")
    window.click(".modal footer button.primary")
    window.wait(lambda: not window.exists(".modal"), "guardar nodo desde la interfaz")
    assert window.read_query("MATCH (p:Person) RETURN p.name") == [["Ana de prueba"]]
    window.wait(lambda: "Ana de prueba" in window.text("main .detail h2"), "mostrar el nodo guardado")
    window.capture(artifacts, "02-nodo-creado")
    first_session = window.invoke("session")
    checks.append("Crear una base con espacios y guardar un nodo desde el formulario.")

    result = window.promise("await window.__TAURI__.event.emit('open-path', arguments[0]);", str(invalid))
    assert result["ok"], result
    window.wait(lambda: "cannot open" in window.text(".toast.error"), "aviso de apertura fallida")
    assert window.invoke("session")["url"] == first_session["url"]
    assert first.name in window.text(".server-pill")
    assert window.read_query("MATCH (p:Person) RETURN p.name") == [["Ana de prueba"]]
    window.capture(artifacts, "03-error-sin-desconexion")
    checks.append("Una apertura fallida muestra el error y conserva la conexión y los datos.")

    window.click(".topbar .ns-menu > button.primary")
    window.click(".menu.right .menu-item:first-child")
    window.wait(lambda: window.exists(".modal"), "abrir un borrador antes del cambio")
    window.type('.modal input[placeholder="Ana García"]', "Borrador de la base anterior")
    window.open_file(second)
    assert not window.exists(".modal"), "el borrador anterior quedó abierto en otra base"
    assert window.read_dom("return window.location.hash;") == "#/overview"
    window.wait(lambda: window.text("main .cards .card:first-child .value") == "0", "mostrar la base vacía")
    assert not window.exists(".toast.error"), "quedó un aviso de la base anterior"
    assert not window.exists("main .error"), "quedó un error de la vista anterior"
    window.capture(artifacts, "04-otra-base-sin-borradores")
    assert window.read_query("MATCH (p:Person) RETURN count(p)") == [[0]]
    stale = window.promise("return await window.__TAURI__.core.invoke('request', arguments[0]);", {
        "method": "POST", "url": first_session["url"] + "/v1/default/query",
        "headers": {"content-type": "application/json"},
        "body": json.dumps({"query": "CREATE (:Person {name: 'No debe aparecer'})"}),
    })
    assert not stale["ok"] and "session changed" in stale["error"], stale
    assert window.read_query("MATCH (p:Person) RETURN count(p)") == [[0]]
    checks.append("Cambiar de base, cerrar borradores y rechazar una escritura de la sesión anterior.")

    window.click('.topbar button[title="Close the database"]')
    window.wait(lambda: window.exists(".start-card"), "cerrar la base")
    selector = f".recent-open[title={json.dumps(str(first))}]"
    window.wait(lambda: window.exists(selector), "archivo en la lista de recientes")
    window.click(selector)
    window.wait(lambda: first.name in window.text(".server-pill")
                and window.exists('.app[aria-busy="false"]'), "reabrir desde recientes")
    assert window.read_query("MATCH (p:Person) RETURN p.name") == [["Ana de prueba"]]
    window.wait(lambda: window.text("main .cards .card:first-child .value") == "1", "mostrar los datos al reabrir")
    assert not window.exists("main .error"), "la vista reabierta muestra un error anterior"
    window.capture(artifacts, "05-base-reabierta")
    checks.append("Cerrar y reabrir desde recientes conservando los datos confirmados.")
    backup = files / "respaldo completo.redb"
    restored = files / "memoria recuperada.redb"
    current = window.invoke("session")
    assert window.invoke("backup_database", {"path": str(backup), "sessionUrl": current["url"]}) > 0
    original = backup.read_bytes()
    for destination in [first, backup]:
        refused = window.promise("return await window.__TAURI__.core.invoke('backup_database', arguments[0]);", {
            "path": str(destination), "sessionUrl": current["url"],
        })
        assert not refused["ok"] and "exists already" in refused["error"], refused
    stale_backup = files / "no debe crearse.redb"
    refused = window.promise("return await window.__TAURI__.core.invoke('backup_database', arguments[0]);", {
        "path": str(stale_backup), "sessionUrl": first_session["url"],
    })
    assert not refused["ok"] and "session changed" in refused["error"], refused
    assert not stale_backup.exists()
    failed_restore = window.promise("return await window.__TAURI__.core.invoke('restore_database', arguments[0]);", {
        "source": str(invalid), "path": str(restored),
    })
    assert not failed_restore["ok"], failed_restore
    assert not restored.exists()
    assert window.invoke("session")["url"] == current["url"]
    assert window.read_query("MATCH (p:Person) RETURN p.name") == [["Ana de prueba"]]
    assert window.invoke("restore_database", {"source": str(backup), "path": str(restored)}) > 0
    assert window.invoke("session")["url"] == current["url"]
    assert backup.read_bytes() == original
    window.open_file(restored)
    assert window.read_query("MATCH (p:Person) RETURN p.name") == [["Ana de prueba"]]
    window.wait(lambda: window.text("main .cards .card:first-child .value") == "1", "mostrar la base recuperada")
    window.capture(artifacts, "06-copia-recuperada")
    checks.append("Copiar y recuperar sin sobrescribir la base activa ni el respaldo, conservando los datos.")
    session = window.invoke("session")
    reply = window.invoke("request", {"method": "GET", "url": session["url"] + "/v1/backup", "headers": {}})
    assert reply["status"] == 200, reply
    assert reply.get("bodyBase64") and reply["body"] == "", "el respaldo binario perdió su representación"
    binary_backup = files / "respaldo del puente.redb"
    binary_backup.write_bytes(base64.b64decode(reply["bodyBase64"], validate=True))
    recovered_from_bridge = files / "recuperada desde el puente.redb"
    assert window.invoke("restore_database", {"source": str(binary_backup), "path": str(recovered_from_bridge)}) > 0
    window.open_file(recovered_from_bridge)
    assert window.read_query("MATCH (p:Person) RETURN p.name") == [["Ana de prueba"]]
    window.wait(lambda: window.text("main .cards .card:first-child .value") == "1", "mostrar la recuperación del puente binario")
    window.capture(artifacts, "07-puente-binario")
    checks.append("Transferir un respaldo binario por el puente Tauri y recuperar sus datos sin alterarlos.")
    for check in checks:
        print(f"OK: {check}", flush=True)
    return checks


def main():
    root = Path(__file__).resolve().parents[3]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=root / "target/debug/grafeno-desktop")
    parser.add_argument("--artifacts", type=Path, default=root / "reports/desktop-e2e")
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"Primero compila la aplicación: no existe {binary}")
    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    port, native_port = free_ports()
    window = Window(f"http://127.0.0.1:{port}")
    driver = None
    result = {"result": "failed", "checks": []}
    with tempfile.TemporaryDirectory(prefix="grafeno-desktop-e2e-") as temporary:
        files = Path(temporary)
        env = os.environ.copy()
        # Aislar historial, preferencias y almacenamiento del WebView.
        for key, directory in [("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
                               ("XDG_CACHE_HOME", "cache")]:
            env[key] = str(files / directory)
        try:
            with (artifacts / "driver.log").open("w") as log:
                driver = subprocess.Popen(["tauri-driver", "--port", str(port),
                                           "--native-port", str(native_port)],
                                          env=env, stdout=log, stderr=subprocess.STDOUT,
                                          start_new_session=True)

                def ready():
                    if driver.poll() is not None:
                        raise RuntimeError("tauri-driver terminó; consulta driver.log")
                    try:
                        window.command("GET", "/status")
                        return True
                    except (OSError, RuntimeError):
                        return False

                window.wait(ready, "iniciar WebDriver")
                session = window.command("POST", "/session", {"capabilities": {"alwaysMatch": {
                    "browserName": "wry", "tauri:options": {"application": str(binary)},
                }}})
                window.session = session["sessionId"]
                result["checks"] = scenario(window, files, artifacts)
                result["result"] = "passed"
        except Exception:
            (artifacts / "error.txt").write_text(traceback.format_exc())
            if window.session:
                try:
                    window.capture(artifacts, "error")
                    (artifacts / "page.html").write_text(window.call("GET", "/source"))
                except Exception:
                    pass
            raise
        finally:
            if window.session:
                try:
                    window.call("DELETE", "")
                except Exception:
                    pass
            window.close()
            if driver is not None:
                try:
                    os.killpg(driver.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    driver.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(driver.pid, signal.SIGKILL)
                    driver.wait()
            (artifacts / "result.json").write_text(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
