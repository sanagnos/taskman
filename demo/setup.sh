#!/usr/bin/env bash
# Sets up a throwaway taskman demo: three fake repositories, projects, tickets and an agent, with
# Jira and Linear issues and PRs read from demo/fixtures (TASKMAN_DEMO) instead of Jira, Linear and
# GitHub. The third project's tracker is a real Local files board. Claude sessions are real and
# run in the fake repositories.
#
# Usage: demo/setup.sh [dir]     (default /tmp/taskman-demo; it's deleted and recreated)
# Then:  source <dir>/env.sh && taskman
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
dir="${1:-/tmp/taskman-demo}"
rm -rf "$dir"
mkdir -p "$dir/state/taskman"

git_init() {
  git -C "$1" init -q -b main
  git -C "$1" -c user.name=Demo -c user.email=demo@example.com add -A
  git -C "$1" -c user.name=Demo -c user.email=demo@example.com commit -q -m "$2"
  echo ".claude/worktrees/" >> "$1/.git/info/exclude"
}

# A GitHub remote that's never contacted: in demo mode it only turns the PR panel on.
remote() { git -C "$1" remote add origin "git@github.com:acme/$(basename "$1").git"; }

# worktree <repo> <key> <branch> [start]: a ticket's worktree in taskman's "inside the project"
# layout, from main or another branch (a stacked ticket). Prints its path.
worktree() {
  local path="$1/.claude/worktrees/$2"
  git -C "$1" worktree add -q -b "$3" "$path" "${4:-main}"
  printf '%s' "$path"
}

commit() { # commit <worktree> <message>
  git -C "$1" -c user.name="Sam Rivera" -c user.email=sam@example.com add -A
  git -C "$1" -c user.name="Sam Rivera" -c user.email=sam@example.com commit -q -m "$2"
}

# ---- checkout-service: a small TypeScript service with the bug ENG-52 is about ----------
cs="$dir/checkout-service"
mkdir -p "$cs/src" "$cs/test"
cat > "$cs/README.md" <<'EOF'
# checkout-service

Orders API: bulk CSV import, payment webhooks, order export.
EOF
cat > "$cs/src/import.ts" <<'EOF'
import { parse } from "./csv";
import { db } from "./db";

export interface OrderRow { sku: string; quantity: number; price: number }

// Imports a CSV of orders. Rows are written as they're parsed, so a bad row
// half way through leaves a partial import behind (ENG-52).
export async function bulkImport(csv: string): Promise<{ imported: number }> {
  const rows = parse(csv);
  const header = rows[0]; // throws on an empty file
  let imported = 0;
  for (const row of rows.slice(1)) {
    const order: OrderRow = { sku: row[0], quantity: Number(row[1]), price: Number(row[2]) };
    await db.orders.insert(order);
    imported++;
  }
  return { imported };
}
EOF
cat > "$cs/src/webhooks.ts" <<'EOF'
// Payment provider webhooks. Failures are retried with backoff (ENG-53).
export async function handleWebhook(event: { id: string; type: string }) {
  return { received: event.id };
}
EOF
cat > "$cs/test/import.e2e.ts" <<'EOF'
import { bulkImport } from "../src/import";

test("imports valid rows", async () => {
  expect(await bulkImport("sku,quantity,price\nA1,2,9.99")).toEqual({ imported: 1 });
});

test("rejects an empty file with a clear error", async () => {
  await expect(bulkImport("")).rejects.toThrow("The file is empty");
});
EOF
git_init "$cs" "Orders API"
remote "$cs"

# ENG-52: rows validated in a first pass; the empty-file case is still missing, so e2e fails.
eng52="$(worktree "$cs" ENG-52 feat/ENG-52-validate-import-rows)"
cat > "$eng52/src/import.ts" <<'EOF'
import { parse } from "./csv";
import { db } from "./db";

export interface OrderRow { sku: string; quantity: number; price: number }
export interface RowError { line: number; reason: string }

// Validates every row first and writes nothing unless all are valid (ENG-52).
export async function bulkImport(csv: string): Promise<{ imported: number } | { errors: RowError[] }> {
  const rows = parse(csv);
  const header = rows[0]; // still throws on an empty file
  const orders: OrderRow[] = [];
  const errors: RowError[] = [];
  rows.slice(1).forEach((row, i) => {
    const order = { sku: row[0], quantity: Number(row[1]), price: Number(row[2]) };
    if (!order.sku) errors.push({ line: i + 2, reason: "missing sku" });
    else if (!(order.quantity > 0)) errors.push({ line: i + 2, reason: "quantity must be positive" });
    else orders.push(order);
  });
  if (errors.length) return { errors };
  await db.orders.insertMany(orders);
  return { imported: orders.length };
}
EOF
commit "$eng52" "ENG-52: validate every row before writing any"

# ENG-53: retries with backoff, done and approved.
eng53="$(worktree "$cs" ENG-53 feat/ENG-53-webhook-retries)"
cat > "$eng53/src/webhooks.ts" <<'EOF'
const MAX_ATTEMPTS = 5;

// Payment provider webhooks: a 5xx from our handler is retried with exponential backoff,
// up to 5 attempts, then alerts (ENG-53).
export async function handleWebhook(event: { id: string; type: string }, process: () => Promise<void>) {
  for (let attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
    try {
      await process();
      return { received: event.id, attempts: attempt };
    } catch (e) {
      if (attempt === MAX_ATTEMPTS) await alert(`Webhook ${event.id} failed ${MAX_ATTEMPTS} times`);
      else await sleep(2 ** attempt * 100);
    }
  }
  return { received: event.id, failed: true };
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function alert(message: string) { console.error(message); }
EOF
cat > "$eng53/test/webhooks.test.ts" <<'EOF'
import { handleWebhook } from "../src/webhooks";

test("retries until the handler succeeds", async () => {
  let calls = 0;
  const result = await handleWebhook({ id: "ev_1", type: "payment.succeeded" }, async () => {
    if (++calls < 3) throw new Error("503");
  });
  expect(result).toEqual({ received: "ev_1", attempts: 3 });
});
EOF
commit "$eng53" "ENG-53: retry failed payment webhooks with backoff"

# ENG-54: stacked on ENG-53, so its worktree starts from ENG-53's branch.
eng54="$(worktree "$cs" ENG-54 feat/ENG-54-page-on-call feat/ENG-53-webhook-retries)"
cat > "$eng54/src/oncall.ts" <<'EOF'
// Pages the payments on-call when a webhook has failed every retry (ENG-54).
export async function pageOnCall(eventId: string, attempts: number) {
  await fetch("https://pager.example.com/v1/incidents", {
    method: "POST",
    body: JSON.stringify({ service: "payments", summary: `Webhook ${eventId} failed ${attempts} times` }),
  });
}
EOF
perl -0pi -e 's/\A/import { pageOnCall } from ".\/oncall";\n\n/; s/^async function alert.*$/async function alert(message: string) { await pageOnCall(message, MAX_ATTEMPTS); }/m' "$eng54/src/webhooks.ts"
commit "$eng54" "ENG-54: page on-call when a webhook keeps failing"

# ---- mobile-app: a small React Native app -------------------------------------------------
ma="$dir/mobile-app"
mkdir -p "$ma/src/cart"
cat > "$ma/README.md" <<'EOF'
# mobile-app

The shop's iOS and Android app.
EOF
cat > "$ma/src/cart/cart.ts" <<'EOF'
export interface CartItem { sku: string; quantity: number }

// Cart changes go straight to the server; with no connection they're lost (APP-7).
export async function addToCart(item: CartItem) {
  await fetch("/api/cart", { method: "POST", body: JSON.stringify(item) });
}
EOF
git_init "$ma" "Shop app"
remote "$ma"

# APP-7: offline queue, work in progress.
app7="$(worktree "$ma" APP-7 sam/app-7-offline-cart)"
cat > "$app7/src/cart/offline.ts" <<'EOF'
import type { CartItem } from "./cart";

// Cart changes made offline wait here and are replayed in order on reconnect (APP-7).
const queue: Array<{ op: "add" | "remove" | "quantity"; item: CartItem }> = [];

export function enqueue(op: "add" | "remove" | "quantity", item: CartItem) {
  queue.push({ op, item });
}

// TODO: replay on reconnect and resolve conflicts with the server's cart.
EOF
commit "$app7" "APP-7: queue cart changes while offline"

# ---- marketing-site: a static site whose tickets are markdown files (Local files) ---------
ms="$dir/marketing-site"
mkdir -p "$ms/pages" "$ms/workplan"/{backlog,todo,doing,review,done}
cat > "$ms/README.md" <<'EOF'
# marketing-site

acme's public website. Tickets live in workplan/, one markdown file each.
EOF
cat > "$ms/pages/pricing.html" <<'EOF'
<h1>Pricing</h1>
<!-- WP-3: plans table goes here -->
EOF
cat > "$ms/workplan/done/WP-1-landing-page.md" <<'EOF'
# Landing page

Hero, three feature blocks and a sign-up button.
EOF
cat > "$ms/workplan/review/WP-2-blog-feed.md" <<'EOF'
---
type: task
assignee: ana
---
# Blog RSS feed

Publish /blog/feed.xml with the latest 20 posts.
EOF
cat > "$ms/workplan/doing/WP-3-pricing-page.md" <<'EOF'
---
type: story
priority: High
assignee: sam
---
# Pricing page

Show the three plans side by side, monthly and yearly, with the yearly saving.

- Plans and prices come from `pricing.json`, not the HTML.
- The yearly toggle remembers its choice.
EOF
cat > "$ms/workplan/todo/WP-4-faq.md" <<'EOF'
# FAQ section

The ten questions support gets most, under the pricing table.
EOF
git_init "$ms" "Website"

# ---- taskman state: projects, tickets, an agent ------------------------------------------
cat > "$dir/state/taskman/tickets.json" <<EOF
{
  "projects": [
    { "name": "checkout-service", "repo": "$cs",
      "tracker": { "provider": "jira", "site": "https://acme.atlassian.net", "key": "ENG" } },
    { "name": "mobile-app", "repo": "$ma",
      "tracker": { "provider": "linear", "site": "acme", "key": "APP" } },
    { "name": "marketing-site", "repo": "$ms",
      "tracker": { "provider": "local", "site": "workplan", "key": "WP" } }
  ],
  "tickets": [
    { "key": "checkout-service-orchestrator", "repo": "$cs", "cwd": "$cs", "agent": "orchestrator" },
    { "key": "ENG-52", "repo": "$cs", "cwd": "$eng52", "branch": "feat/ENG-52-validate-import-rows" },
    { "key": "ENG-53", "repo": "$cs", "cwd": "$eng53", "branch": "feat/ENG-53-webhook-retries" },
    { "key": "ENG-54", "repo": "$cs", "cwd": "$eng54", "branch": "feat/ENG-54-page-on-call",
      "base": { "key": "ENG-53", "branch": "feat/ENG-53-webhook-retries" } },
    { "key": "ENG-58", "repo": "$cs", "cwd": "$cs" },
    { "key": "APP-7", "repo": "$ma", "cwd": "$app7", "branch": "sam/app-7-offline-cart" },
    { "key": "APP-9", "repo": "$ma", "cwd": "$ma" },
    { "key": "WP-3", "repo": "$ms", "cwd": "$ms" }
  ],
  "selected": 1
}
EOF
# No accounts, and no background updates replacing the demo build.
echo '{ "accounts": [], "auto_update": false }' > "$dir/config.json"

cat > "$dir/env.sh" <<EOF
# Source this to run taskman against the demo.
export XDG_STATE_HOME="$dir/state"
export TASKMAN_CONFIG="$dir/config.json"
export TASKMAN_DEMO="$repo_root/demo/fixtures"
export PATH="$repo_root/target/debug:\$PATH"
EOF

(cd "$repo_root" && cargo build --quiet)
echo "Demo ready in $dir"
echo "Run: source $dir/env.sh && taskman"
echo "First time only: Claude asks whether to trust each demo folder; accept it once (see demo/README.md)."
