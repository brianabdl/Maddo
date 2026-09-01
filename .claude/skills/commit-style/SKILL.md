---
name: commit-style
description: >
  Write git commit messages following the Conventional Commits v1.0.0
  specification (type(scope)!: description, body, footers, BREAKING CHANGE).
  Use when user says "commit", "commit message", "conventional commit",
  "commit style", or asks to write/format a commit per Conventional Commits.
user-invokable: true
argument-hint: "[optional: message hint]"
license: MIT
---

# Commit Style (Conventional Commits v1.0.0)

Generate commit messages that follow the Conventional Commits spec exactly.
Source: https://www.conventionalcommits.org/en/v1.0.0/

## Structure

```
<type>[optional scope][!]: <description>

[optional body]

[optional footer(s)]
```

## Types

- `feat` — new feature (SemVer MINOR)
- `fix` — bug fix (SemVer PATCH)
- Any other noun allowed: `build`, `chore`, `ci`, `docs`, `style`, `refactor`,
  `perf`, `test`, etc. Match whatever type convention the repo already uses
  (check `git log --oneline -20`); don't invent new types if the repo has a
  consistent set.

## Spec rules (verbatim, numbered 1-16 in the spec)

1. Commit prefixed with type (noun), optional scope, optional `!`, required `: `.
2. `feat` must be used when a commit adds a new feature.
3. `fix` must be used when a commit fixes a bug.
4. Scope is an optional parenthetical noun after type, describing a section
   of the codebase, e.g. `fix(parser):`.
5. Description immediately follows the `type/scope: ` prefix — short summary.
6. A longer body may follow the description, after one blank line.
7. Body is free-form, may have multiple newline-separated paragraphs.
8. Footers follow the body after one blank line, each as one token, then
   either `: ` or ` #` as separator, then a value.
9. Footer token uses hyphens instead of spaces (e.g. `Acked-by:`), except
   `BREAKING CHANGE`, which stays as two words.
10. Footer value may contain spaces/newlines; parsing stops at the next
    valid `token: ` / `token #` pair.
11. Breaking change indicated by `!` before the `:` in the prefix, or by a
    `BREAKING CHANGE:` footer, or both.
12. If used as footer, must be exactly `BREAKING CHANGE: <description>`.
13. If `!` used in prefix, `BREAKING CHANGE:` footer is optional; description
    right after prefix must describe the breaking change.
14. `!` draws attention to breaking change; commit MUST then be a MAJOR bump.
15. Types are case-insensitive except `BREAKING CHANGE`, which MUST be
    uppercase.
16. `BREAKING-CHANGE` MUST be synonymous with `BREAKING CHANGE` as a footer token.

## Workflow

1. Run `git status` and `git diff --staged` (fall back to `git diff` if
   nothing staged, but flag that to the user — don't commit unstaged work
   silently).
2. Determine `type` from what actually changed (new capability → `feat`;
   bug fix → `fix`; docs-only → `docs`; pure refactor with no behavior
   change → `refactor`; etc.). Don't guess when the diff is mixed — pick
   the dominant change and mention the rest in the body.
3. Determine `scope` from the primary module/dir touched, only if it adds
   clarity (skip scope if the change spans the whole repo).
4. Write `description`: imperative mood, no trailing period, lowercase
   unless a proper noun starts it, short enough to read on one line.
5. Add a `body` only when the "why" isn't obvious from the diff/description
   alone — reference the motivating bug, constraint, or decision.
6. If the change breaks a public API/CLI/config contract: add `!` after
   type/scope and either explain inline or add a `BREAKING CHANGE:` footer.
7. Add other footers only if relevant (`Refs:`, `Closes:`, etc.) — don't
   invent footers nobody asked for. Never add `Co-authored-by:` or any
   other co-author mention, even if a tool/template suggests one.
8. Before finalizing, re-check against rules 1-16 above.

## Example outputs

```
feat(auth): add refresh-token rotation

Old tokens stayed valid after rotation, letting a leaked token be reused
indefinitely. Rotation now invalidates the prior token server-side.

BREAKING CHANGE: `POST /auth/refresh` now returns a new refresh token in
the body; clients must persist it and stop reusing the old one.
```

```
fix(parser)!: correct off-by-one in line numbering

Refs: #482
```

```
chore: bump dependency versions
```
