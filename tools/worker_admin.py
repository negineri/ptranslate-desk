#!/usr/bin/env python3
"""仲介の端末でワーカーの状態だけを操作する。HTTP通信は行わない。"""

import argparse
import json
import os
import re
from pathlib import Path
import sqlite3
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "server"))
import translator as tr  # noqa: E402


def main(argv=None):
    parser = argparse.ArgumentParser(
        description="ワーカーの検疫・失効をローカルのSQLiteで操作します"
    )
    parser.add_argument(
        "--state-file",
        default=os.environ.get("TR_STATE_FILE", ""),
        help="TR_STATE_FILEと同じSQLiteファイル",
    )
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("list-owners", help="所有者・登録記録数・検疫と失効の状態を表示")
    for name in ("revoke-owner", "release-owner"):
        command = commands.add_parser(name)
        command.add_argument("name")
    command = commands.add_parser("revoke-worker")
    command.add_argument("worker_id")
    args = parser.parse_args(argv)
    registry = None
    try:
        if not args.state_file or not Path(args.state_file).is_file():
            raise ValueError("既存の状態ファイルを指定してください")
        if args.command in ("revoke-owner", "release-owner") and not tr.USER_RE.fullmatch(
            args.name
        ):
            raise ValueError("所有者名が不正です")
        if args.command == "revoke-worker" and not re.fullmatch(r"[0-9a-f]{32}", args.worker_id):
            raise ValueError("ワーカーIDが不正です")
        registry = tr.WorkerRegistry(args.state_file)
        if args.command == "list-owners":
            with registry.lock:
                rows = registry.db.execute(
                    "SELECT o.name,count(w.id),o.quarantined,o.revoked FROM owners o LEFT JOIN workers w ON w.owner=o.name GROUP BY o.name ORDER BY o.name"
                ).fetchall()
            print(
                json.dumps(
                    [
                        {
                            "owner": row[0],
                            "workers": row[1],
                            "quarantined": bool(row[2]),
                            "revoked": bool(row[3]),
                        }
                        for row in rows
                    ],
                    ensure_ascii=False,
                )
            )
        elif args.command == "revoke-owner":
            registry.revoke_owner(args.name)
            print(json.dumps({"updated": True}))
        elif args.command == "revoke-worker":
            if not registry.set_state(args.worker_id, "revoked"):
                raise ValueError("対象が見つかりません")
            print(json.dumps({"updated": True}))
        else:
            with registry.lock:
                if not registry.db.execute(
                    "SELECT 1 FROM owners WHERE name=?", (args.name,)
                ).fetchone():
                    raise ValueError("対象が見つかりません")
            registry.release_owner(args.name)
            print(json.dumps({"updated": True}))
        return 0
    except (ValueError, OSError, sqlite3.Error, tr.HttpError) as error:
        # 例外にはパスや資格が含まれ得るので型以外は出さない。
        print(f"操作に失敗しました ({type(error).__name__})", file=sys.stderr)
        return 1
    finally:
        if registry is not None:
            registry.close()


if __name__ == "__main__":
    raise SystemExit(main())
