#!/usr/bin/env bash
# Auto-commit and push every change in this repository.
# Invoked by the Claude Code Stop hook (.claude/settings.json).
set -u

PROJECT_DIR="${CLAUDE_PROJECT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
cd "$PROJECT_DIR" 2>/dev/null || exit 0

BRANCH="$(git rev-parse --abbrev-ref HEAD 2>/dev/null)"
if [ -z "$BRANCH" ] || [ "$BRANCH" = "HEAD" ]; then
  exit 0  # not a git repo, or detached HEAD
fi

# Don't create commits in the middle of a merge/rebase.
if [ -d .git/rebase-merge ] || [ -d .git/rebase-apply ] || [ -f .git/MERGE_HEAD ]; then
  echo '{"systemMessage":"Автоотправка пропущена: в репозитории идёт merge/rebase."}'
  exit 0
fi

git add -A
if git diff --cached --quiet; then
  exit 0  # nothing to send
fi

git commit -q -m "auto: $(date '+%Y-%m-%d %H:%M:%S')" \
  -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>" || {
  echo '{"systemMessage":"Автоотправка: коммит не создан (см. вывод git)."}'
  exit 0
}

if git push -q origin "$BRANCH" 2>/dev/null; then
  exit 0
fi

# Push rejected (remote moved ahead) — rebase and retry once.
if git pull -q --rebase --autostash origin "$BRANCH" >/dev/null 2>&1 \
  && git push -q origin "$BRANCH" 2>/dev/null; then
  echo '{"systemMessage":"Автоотправка: изменения запушены после rebase."}'
  exit 0
fi

echo '{"systemMessage":"Автоотправка не удалась: запустите git push вручную."}'
exit 0
