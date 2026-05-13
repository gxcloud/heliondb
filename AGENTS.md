# Agent Contribution Rules

This file defines how coding agents (opencode, etc.) should interact with this repository.

## Branch Strategy

- **`main` is the release branch** — never commit or push to it directly.
- Always create a feature branch from `main` before making changes.
- Branch naming convention: `feat/<short-description>` or `fix/<short-description>`.

## Workflow

1. Create a new branch:
   ```
   git checkout -b feat/<description>
   ```

2. Make changes and commit them on the feature branch.

3. When the feature is complete, push the branch and create a pull request:
   ```
   git push -u origin <branch-name>
   gh pr create --title "<type>: <description>" --body "<summary>"
   ```

4. After the PR is merged, sync local `main`:
   ```
   git switch main && git pull
   ```

## Enforcement

Branch protection rules are configured on the GitHub repository settings:

- **Require a pull request before merging** is enabled for `main`.
- Direct pushes to `main` are rejected at the server level.
