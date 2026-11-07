#!/usr/bin/env python3
"""手元の署名鍵を保護Environmentへ登録し、旧Secretsを削除する。"""

import argparse
import base64
from getpass import getpass, GetPassWarning
import json
from pathlib import Path
import subprocess
import warnings

REPOSITORY = "negineri/ptranslate-desk"
ENVIRONMENT = "release-signing"
NAMES = (
    "MACOS_SIGNING_CERTIFICATE",
    "MACOS_SIGNING_PASSWORD",
    "TAURI_SIGNING_PRIVATE_KEY",
    "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
)


def gh(*args, value=None):
    command = ["gh", *args]
    if args[0] != "api":
        command += ["--repo", REPOSITORY]
    result = subprocess.run(
        command,
        input=value,
        capture_output=True,
        text=True,
        timeout=60,
    )
    if result.returncode:
        # 入力された秘密やAPIの診断文を表示しない。
        raise ValueError("GitHubの操作に失敗しました。ghの認証と権限を確認してください")
    return result.stdout


def register(certificate, updater_key):
    environment = json.loads(gh("api", f"repos/{REPOSITORY}/environments/{ENVIRONMENT}"))
    policies = json.loads(
        gh("api", f"repos/{REPOSITORY}/environments/{ENVIRONMENT}/deployment-branch-policies")
    )
    if (
        environment.get("can_admins_bypass", True)
        or not any(
            rule.get("type") == "required_reviewers" and rule.get("reviewers")
            for rule in environment.get("protection_rules", [])
        )
        or environment.get("deployment_branch_policy")
        != {"protected_branches": False, "custom_branch_policies": True}
        or [(p.get("name"), p.get("type")) for p in policies.get("branch_policies", [])]
        != [("v*", "tag")]
    ):
        raise ValueError("Environmentの承認・タグ制限が未設定です。秘密は登録しません")
    values = {
        "MACOS_SIGNING_CERTIFICATE": base64.b64encode(certificate.read_bytes()).decode(),
        "TAURI_SIGNING_PRIVATE_KEY": updater_key.read_text().strip(),
        "MACOS_SIGNING_PASSWORD": getpass("p12のパスワード（表示しません）: "),
        "TAURI_SIGNING_PRIVATE_KEY_PASSWORD": getpass("更新署名鍵のパスワード（表示しません）: "),
    }
    if not all(values.values()):
        raise ValueError("空の鍵・パスワードは登録できません")
    for name in NAMES:
        gh("secret", "set", name, "--env", ENVIRONMENT, value=values[name])
        print(f"Environmentへ登録済み: {name}")
    actual = {
        item["name"]
        for item in json.loads(gh("secret", "list", "--env", ENVIRONMENT, "--json", "name"))
    }
    if not set(NAMES).issubset(actual):
        raise ValueError("Environmentへの登録を確認できません。旧Secretsは削除しません")
    # 全件の登録成功を確認するまで、旧Secretsを1つも削除しない。
    old = {item["name"] for item in json.loads(gh("secret", "list", "--json", "name"))}
    for name in NAMES:
        if name in old:
            gh("secret", "delete", name)
            print(f"リポジトリSecretsから削除済み: {name}")
    print("署名Secretsの移行が完了しました")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--certificate", type=Path, required=True, help="既存のp12ファイル")
    parser.add_argument("--updater-key", type=Path, required=True, help="既存のTauri秘密鍵ファイル")
    args = parser.parse_args()
    try:
        # 非対話環境でパスワード入力が平文エコーへ切り替わるのを拒否する。
        with warnings.catch_warnings():
            warnings.simplefilter("error", GetPassWarning)
            register(args.certificate, args.updater_key)
    except GetPassWarning:
        parser.exit(1, "パスワードを非表示にできる対話端末から実行してください\n")
    except (ValueError, OSError, subprocess.TimeoutExpired) as error:
        message = (
            str(error)
            if isinstance(error, ValueError)
            else "ファイル読み取り・GitHub操作に失敗しました"
        )
        parser.exit(1, message + "\n")


if __name__ == "__main__":
    main()
