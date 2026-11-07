#!/usr/bin/env python3
"""固定の自己署名証明書でmacOSのアプリ識別条件を保つ。"""

import argparse
import base64
import json
import os
from pathlib import Path
import secrets
import shlex
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
PUBLIC = ROOT / "desktop/signing"
IDENTIFIER = "com.witenp.ptranslate-desk"


def run(*args, env=None):
    result = subprocess.run(args, env=env, text=True, capture_output=True)
    if result.returncode:
        raise ValueError(f"{args[0]}: {result.stderr.strip()}")
    return result.stdout


def fingerprint(certificate):
    value = run("openssl", "x509", "-in", str(certificate), "-noout", "-fingerprint", "-sha1")
    return value.split("=", 1)[1].strip().replace(":", "").upper()


def create(directory):
    directory.mkdir(mode=0o700, parents=True, exist_ok=False)
    password = secrets.token_hex(32)
    password_file = directory / "password.txt"
    password_file.write_text(password)
    password_file.chmod(0o600)
    config = directory / "certificate.cnf"
    config.write_text(
        "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n"
        "[dn]\nCN=Personal Apps Signing Test\nO=Personal Apps\n"
        "[ext]\nbasicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature\nextendedKeyUsage=critical,codeSigning\n"
    )
    env = {**os.environ, "PTRANSLATE_CERT_PASSWORD": password}
    run(
        "openssl",
        "req",
        "-x509",
        "-newkey",
        "rsa:3072",
        "-days",
        "7300",
        "-config",
        str(config),
        "-keyout",
        str(directory / "private-key.pem"),
        "-out",
        str(directory / "certificate.pem"),
        "-passout",
        "env:PTRANSLATE_CERT_PASSWORD",
        env=env,
    )
    # macOSのPKCS#12インポートと互換性のある暗号形式を明示する。
    run(
        "openssl",
        "pkcs12",
        "-export",
        "-keypbe",
        "PBE-SHA1-3DES",
        "-certpbe",
        "PBE-SHA1-3DES",
        "-macalg",
        "sha1",
        "-inkey",
        str(directory / "private-key.pem"),
        "-in",
        str(directory / "certificate.pem"),
        "-out",
        str(directory / "certificate.p12"),
        "-passin",
        "env:PTRANSLATE_CERT_PASSWORD",
        "-passout",
        "env:PTRANSLATE_CERT_PASSWORD",
        env=env,
    )
    for name in ("private-key.pem", "certificate.p12"):
        (directory / name).chmod(0o600)
    return directory / "certificate.pem"


def import_identity(directory, certificate, data, password):
    keychain = directory / "signing.keychain-db"
    state = directory / "keychains.json"
    original = shlex.split(run("security", "list-keychains", "-d", "user"))
    state.write_text(json.dumps(original))
    archive = directory / "certificate.p12"
    archive.write_bytes(data)
    archive.chmod(0o600)
    keychain_password = secrets.token_hex(32)
    run("security", "create-keychain", "-p", keychain_password, str(keychain))
    run("security", "list-keychains", "-d", "user", "-s", *original, str(keychain))
    run("security", "unlock-keychain", "-p", keychain_password, str(keychain))
    run("security", "set-keychain-settings", "-lut", "21600", str(keychain))
    run(
        "security",
        "import",
        str(archive),
        "-k",
        str(keychain),
        "-P",
        password,
        "-T",
        "/usr/bin/codesign",
    )
    run(
        "security",
        "set-key-partition-list",
        "-S",
        "apple-tool:,apple:,codesign:",
        "-s",
        "-k",
        keychain_password,
        str(keychain),
    )
    archive.unlink()
    identity = fingerprint(certificate)
    identities = run("security", "find-identity", "-p", "codesigning", str(keychain))
    if identity not in identities:
        raise ValueError("秘密鍵が公開証明書と一致しません")
    # ビルド用のMacに限り、コード署名用途で信頼する。利用者のMacには追加しない。
    trust_scope = (
        ["sudo", "security", "add-trusted-cert", "-d"]
        if os.environ.get("GITHUB_ACTIONS") == "true"
        else ["security", "add-trusted-cert"]
    )
    run(
        *trust_scope,
        "-r",
        "trustRoot",
        "-p",
        "codeSign",
        "-k",
        str(keychain),
        str(certificate),
    )
    return identity


def cleanup(directory, certificate):
    state = directory / "keychains.json"
    if not state.exists():
        return
    trust_scope = (
        ["sudo", "security", "remove-trusted-cert", "-d"]
        if os.environ.get("GITHUB_ACTIONS") == "true"
        else ["security", "remove-trusted-cert"]
    )
    subprocess.run([*trust_scope, str(certificate)], capture_output=True)
    run("security", "list-keychains", "-d", "user", "-s", *json.loads(state.read_text()))
    keychain = directory / "signing.keychain-db"
    if keychain.exists():
        run("security", "delete-keychain", str(keychain))


def verify(app, certificate, identifier=IDENTIFIER):
    expected = (
        f'identifier "{identifier}" and certificate root = H"{fingerprint(certificate).lower()}"'
    )
    run("codesign", "--verify", "--deep", "--strict", "-R", "=" + expected, str(app))
    result = subprocess.run(
        ["codesign", "-d", "-r-", str(app)], text=True, capture_output=True, check=True
    )
    requirement = (
        (result.stdout + result.stderr).split("designated => ", 1)[1].splitlines()[0].strip()
    )
    if requirement != expected:
        raise ValueError("ビルドの識別条件が固定の証明書とアプリIDに一致しません")
    return requirement


def self_test():
    with tempfile.TemporaryDirectory(prefix="ptranslate-signing-") as temp:
        directory = Path(temp) / "identity"
        certificate = create(directory)
        try:
            identity = import_identity(
                directory,
                certificate,
                (directory / "certificate.p12").read_bytes(),
                (directory / "password.txt").read_text(),
            )
            requirements = []
            for source in ("true", "false"):
                target = directory / source
                shutil.copyfile("/usr/bin/" + source, target)
                run(
                    "codesign",
                    "--force",
                    "--sign",
                    identity,
                    "--timestamp=none",
                    "--identifier",
                    "com.example.signing-test",
                    str(target),
                )
                requirements.append(verify(target, certificate, "com.example.signing-test"))
            if requirements[0] != requirements[1]:
                raise ValueError("更新前後の識別条件が異なります")
            run("codesign", "--force", "--sign", "-", str(target))
            try:
                verify(target, certificate, "com.example.signing-test")
            except ValueError:
                pass
            else:
                raise ValueError("アドホック署名への置き換えを検出できませんでした")
            print("異なるビルドの識別条件一致・異なる署名の拒否: PASS")
        finally:
            cleanup(directory, certificate)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["import", "cleanup", "verify", "self-test"])
    parser.add_argument("--directory", type=Path)
    parser.add_argument("--app", type=Path)
    args = parser.parse_args()
    directory = args.directory
    try:
        if args.action == "self-test":
            self_test()
        elif args.action == "verify":
            if not args.app:
                parser.error("--app が必要です")
            print(verify(args.app, PUBLIC / "certificate.pem"))
        elif not directory:
            parser.error("--directory が必要です")
        elif args.action == "import":
            if not os.environ.get("MACOS_SIGNING_CERTIFICATE") or not os.environ.get(
                "MACOS_SIGNING_PASSWORD"
            ):
                parser.error(
                    "MACOS_SIGNING_CERTIFICATE と MACOS_SIGNING_PASSWORD を設定してください"
                )
            directory.mkdir(mode=0o700, parents=True)
            identity = import_identity(
                directory,
                PUBLIC / "certificate.pem",
                base64.b64decode(os.environ["MACOS_SIGNING_CERTIFICATE"], validate=True),
                os.environ["MACOS_SIGNING_PASSWORD"],
            )
            with Path(os.environ["GITHUB_ENV"]).open("a") as output:
                output.write(f"APPLE_SIGNING_IDENTITY={identity}\n")
        elif args.action == "cleanup":
            cleanup(directory, PUBLIC / "certificate.pem")
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        # subprocessの引数にはパスワードがあるので、CalledProcessErrorの全文は出さない。
        message = (
            "コマンドの実行に失敗しました"
            if isinstance(error, subprocess.CalledProcessError)
            else str(error)
        )
        parser.exit(1, f"署名の処理に失敗しました: {message}\n")


if __name__ == "__main__":
    main()
