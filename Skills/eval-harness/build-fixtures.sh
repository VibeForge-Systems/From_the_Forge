#!/usr/bin/env bash
# Builds throwaway fixture repositories for the skill evals. Everything
# credential-shaped is generated at run time and lives only under $FX.
set -euo pipefail
FX="${1:?fixtures dir}"
REPO="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
VG="$REPO/Skills/vibeforge-gate/skills/vibeforge-gate"
rm -rf "$FX"; mkdir -p "$FX"
rand() { LC_ALL=C tr -dc "$1" < /dev/urandom | head -c "$2"; }
gi() { git -C "$1" -c user.name="$2" -c user.email="$3" -c commit.gpgsign=false "${@:4}"; }

# ---------------------------------------------------------------- node base
node_base() {  # DIR
  local d="$1"; mkdir -p "$d/src" "$d/test" "$d/.github/workflows"
  cat > "$d/package.json" <<'EOF'
{
  "name": "retry-kit",
  "version": "0.4.2",
  "private": true,
  "type": "module",
  "scripts": {
    "lint": "eslint .",
    "test": "node --test",
    "check": "npm run lint && npm test"
  },
  "devDependencies": {
    "eslint": "9.17.0"
  },
  "engines": { "node": ">=22" }
}
EOF
  cat > "$d/src/index.js" <<'EOF'
export async function withRetry(fn, { attempts = 3, baseMs = 50 } = {}) {
  let lastErr;
  for (let i = 0; i < attempts; i++) {
    try {
      return await fn(i);
    } catch (err) {
      lastErr = err;
      await new Promise((r) => setTimeout(r, baseMs * 2 ** i));
    }
  }
  throw lastErr;
}
EOF
  cat > "$d/test/index.test.js" <<'EOF'
import test from "node:test";
import assert from "node:assert/strict";
import { withRetry } from "../src/index.js";

test("retries until success", async () => {
  let n = 0;
  const v = await withRetry(async () => { if (n++ < 2) throw new Error("x"); return "ok"; }, { baseMs: 1 });
  assert.equal(v, "ok");
  assert.equal(n, 3);
});
EOF
  cat > "$d/eslint.config.js" <<'EOF'
export default [{ files: ["**/*.js"], rules: { "no-unused-vars": "error", "prefer-const": "error" } }];
EOF
  cat > "$d/Makefile" <<'EOF'
.PHONY: check lint test
check: lint test
lint:
	npm run lint
test:
	npm test
EOF
  cat > "$d/CONTRIBUTING.md" <<'EOF'
# Contributing

- Run `npm run lint` and `npm test` before committing. CI runs the same two.
- Keep `package-lock.json` in sync with `package.json`.
- No secrets in the tree, ever. Use `.env.local` (gitignored).
EOF
  printf '.env.local\nnode_modules/\n' > "$d/.gitignore"
  cat > "$d/.github/workflows/ci.yml" <<'EOF'
name: ci
on:
  pull_request:
  push:
    branches: [main]
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with: { node-version: 22 }
      - run: npm ci
      - run: npm run lint
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with: { node-version: 22 }
      - run: npm ci
      - run: npm test
  secrets:
    # Secret scanning used to run on the paid tier; this job was added when it stopped.
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 0 }
      - uses: gitleaks/gitleaks-action@v2
        env: { GITHUB_TOKEN: "${{ secrets.GITHUB_TOKEN }}" }
EOF
  printf '# retry-kit\n\nSmall retry helper with exponential backoff.\n' > "$d/README.md"
}

init_with_remote() {  # DIR
  local d="$1"
  git -C "$d" init -q -b main
  gi "$d" "Dana Developer" dana@example.com add -A
  gi "$d" "Dana Developer" dana@example.com commit -qm "feat: initial retry helper"
  git init -q --bare "$d.git"
  git -C "$d" remote add origin "$d.git"
  git -C "$d" push -q origin main
}

# 1. ci-node: hosted CI only, no local gate
node_base "$FX/ci-node"; init_with_remote "$FX/ci-node"

# 2. gated-node: has a vibeforge gate; gitleaks 8.30.1 resolves from the cache
gated() {  # DIR PIN WITH_FETCH
  local d="$1" pin="$2" fetch="$3"
  node_base "$d"
  mkdir -p "$d/.vibeforge/hooks"
  cp "$VG/templates/gate.sh" "$d/.vibeforge/gate.sh"
  cp "$VG/templates/hooks/pre-commit" "$VG/templates/hooks/pre-push" "$d/.vibeforge/hooks/"
  chmod +x "$d/.vibeforge/gate.sh" "$d/.vibeforge/hooks/"*
  {
    cat <<'EOF'
# retry-kit merge bar. NOT COVERED: cross-OS matrix, npm audit (no network in the gate).
meta:
  name: retry-kit
  default_stage: push
  path_prepend: ./node_modules/.bin

checks:
  - id: no-merge-markers
    desc: no unresolved conflict markers
    stage: commit
    why: |
      A leftover <<<<<<< is a build break that review misses more often than it should.
    run: |
      ! git grep -nE '^(<{7}|={7}|>{7})( |$)' -- . ':!*.md'

  - id: lint
    desc: eslint, repo config
    stage: commit
    needs: eslint
    install: "npm ci"
    why: |
      The same rule set CI ran. The lockfile pins eslint, so no tool_version here.
    run: |
      eslint .

  - id: test
    desc: unit tests
    stage: commit
    why: |
      node --test needs nothing installed.
    run: |
      node --test

  - id: slow-tests
    desc: the slow integration pass
    stage: push
    skip_env: SKIP_SLOW
    why: |
      Four minutes on a laptop. Deliberately skippable for docs-only pushes.
    run: |
      sleep 1; echo "integration ok"

EOF
    # secret-scan: the catalog's tree scan, pinned per argument
    awk '/^  - id: secret-scan$/,/^$/' "$VG/catalog/supply-chain.yaml" \
      | sed "s/tool_version: \"8.30.1\"/tool_version: \"$pin\"/; s/v8.30.1/v$pin/g"
    # denylist-scan from the catalog, verbatim
    awk '/^  - id: denylist-scan$/,/^$/' "$VG/catalog/supply-chain.yaml"
  } > "$d/.vibeforge/gates.yaml"
  if [ "$fetch" = no ]; then
    # drop the fetch block so a missing pinned build is a SKIP, not a download
    python3 -I - "$d/.vibeforge/gates.yaml" <<'PY'
import re,sys
p=sys.argv[1]; s=open(p).read()
s=re.sub(r"    fetch_sha256: .*\n", "", s, count=1)
s=re.sub(r"    fetch: \|\n(?:      .*\n)+", "", s, count=1)
open(p,'w').write(s)
PY
  fi
  init_with_remote "$d"
}
gated "$FX/gated-node" "8.30.1" yes
gated "$FX/gated-node-skip" "8.28.0" no

# gated-node: a branch ahead of origin with a token added then removed
d="$FX/gated-node"
git -C "$d" checkout -q -b feat/retry-jitter
mkdir -p "$d/config"
printf 'GH_TOKEN=ghp_%s\n' "$(rand 'A-Za-z0-9' 36)" > "$d/config/dev.env"
sed -i 's/baseMs \* 2 \*\* i/baseMs * 2 ** i + Math.random() * 10/' "$d/src/index.js"
gi "$d" "Dana Developer" dana@example.com add -A
gi "$d" "Dana Developer" dana@example.com commit -qm "feat: add jitter to backoff"
git -C "$d" rm -q config/dev.env
gi "$d" "Dana Developer" dana@example.com commit -qm "chore: drop stray env file"

# ---------------------------------------------------------------- side-tool (rpr)
st="$FX/side-tool"; mkdir -p "$st/src" "$st/deploy"
cat > "$st/README.md" <<'EOF'
# side-tool

Syncs build artifacts from Jenkins to S3. Internal use.

Jenkins lives at https://jenkins.acme.corp (10.42.1.7); ask #platform for access.
EOF
cat > "$st/src/sync.py" <<'EOF'
import os, boto3

def main():
    s3 = boto3.client("s3")
    bucket = os.environ["ARTIFACT_BUCKET"]
    for name in os.listdir("out"):
        s3.upload_file(os.path.join("out", name), bucket, name)

if __name__ == "__main__":
    main()
EOF
printf 'boto3==1.35.0\n' > "$st/requirements.txt"
printf 'out/\n__pycache__/\n' > "$st/.gitignore"
git -C "$st" init -q -b main
gi "$st" "Toby S." toby@acme-corp.example add -A
gi "$st" "Toby S." toby@acme-corp.example commit -qm "initial sync script"
cat > "$st/.env" <<EOF
AWS_ACCESS_KEY_ID=AKIA$(rand 'A-Z0-9' 16)
AWS_SECRET_ACCESS_KEY=$(rand 'A-Za-z0-9/+' 40)
ARTIFACT_BUCKET=acme-build-artifacts
EOF
gi "$st" "Toby S." toby@acme-corp.example add -A
gi "$st" "Toby S." toby@acme-corp.example commit -qm "add env for local runs"
git -C "$st" rm -q .env
printf '.env\n' >> "$st/.gitignore"
gi "$st" "nagual69" nagual69@users.noreply.github.com add -A
gi "$st" "nagual69" nagual69@users.noreply.github.com commit -qm "remove .env, gitignore it"
cat > "$st/deploy/notes.md" <<'EOF'
# Deploy notes

Artifacts land in s3://acme-build-artifacts and are mirrored to \\fileserver01\builds
for the Initech engagement team.
EOF
gi "$st" "nagual69" nagual69@users.noreply.github.com add -A
gi "$st" "nagual69" nagual69@users.noreply.github.com commit -qm "deploy notes"
git -C "$st" tag -a v0.1.0 -m "first internal release"
# a PR ref with a token only there (public after a flip)
git -C "$st" checkout -q -b pr-3
printf 'GITHUB_TOKEN=ghp_%s\n' "$(rand 'A-Za-z0-9' 36)" > "$st/deploy/ci.env"
gi "$st" "Toby S." toby@acme-corp.example add -A
gi "$st" "Toby S." toby@acme-corp.example commit -qm "wip: ci token for testing"
git -C "$st" checkout -q main
git init -q --bare "$st.git"
git -C "$st" push -q "$st.git" main v0.1.0
git -C "$st" push -q "$st.git" pr-3:refs/pull/3/head
git -C "$st" branch -q -D pr-3
git -C "$st" remote add origin "$st.git"
git -C "$st" fetch -q origin
git -C "$st" branch -q -u origin/main main
# an already-rewritten clone and an empty "new public" repo, for the publish eval
git clone -q --no-local "$st.git" "$FX/side-tool-rewritten"
( cd "$FX/side-tool-rewritten" && git filter-repo --quiet --sensitive-data-removal --no-fetch --invert-paths --path .env >/dev/null 2>&1 )
git init -q --bare "$FX/new-public.git"
printf 'toby@acme-corp.example\nnagual69@users.noreply.github.com\n' > "$FX/identities-work.txt"

# ---------------------------------------------------------------- express-prisma (coroner)
ep="$FX/express-prisma/packages/api"; mkdir -p "$ep/src/routes" "$ep/src/services" "$ep/src/db" "$ep/src/auth" "$ep/prisma"
cat > "$FX/express-prisma/package.json" <<'EOF'
{ "name": "shopfront", "private": true, "workspaces": ["packages/*"] }
EOF
cat > "$FX/express-prisma/CLAUDE.md" <<'EOF'
# shopfront

Express API + Prisma on Postgres (pgvector for product search). Deployed as a
long-lived Node container (see packages/api/Dockerfile). House rule: raw SQL is
allowed only in src/services/search.ts.
EOF
cat > "$ep/package.json" <<'EOF'
{
  "name": "@shopfront/api",
  "version": "2.3.0",
  "scripts": { "dev": "tsx src/server.ts", "test": "vitest run" },
  "dependencies": {
    "@prisma/client": "6.1.0",
    "express": "4.21.2",
    "openai": "4.77.0",
    "zod": "3.24.1"
  },
  "devDependencies": { "prisma": "6.1.0", "tsx": "4.19.2", "typescript": "5.7.2", "vitest": "2.1.8" }
}
EOF
cat > "$ep/tsconfig.json" <<'EOF'
{ "compilerOptions": { "strict": false, "target": "ES2022", "module": "NodeNext", "outDir": "dist" } }
EOF
cat > "$ep/Dockerfile" <<'EOF'
FROM node:22-slim
WORKDIR /app
COPY . .
RUN npm ci && npx prisma generate
CMD ["node", "dist/server.js"]
EOF
cat > "$ep/prisma/schema.prisma" <<'EOF'
datasource db {
  provider   = "postgresql"
  url        = env("DATABASE_URL")
  extensions = [vector]
}

generator client {
  provider        = "prisma-client-js"
  previewFeatures = ["postgresqlExtensions"]
}

model Tenant {
  id       String    @id @default(cuid())
  name     String
  users    User[]
  products Product[]
  orders   Order[]
}

model User {
  id       String  @id @default(cuid())
  email    String  @unique
  role     String  @default("member")
  tenantId String
  tenant   Tenant  @relation(fields: [tenantId], references: [id])
  orders   Order[]
}

model Product {
  id          String                 @id @default(cuid())
  tenantId    String
  tenant      Tenant                 @relation(fields: [tenantId], references: [id])
  name        String
  description String
  embedding   Unsupported("vector(1536)")?
  items       OrderItem[]
}

model Order {
  id        String      @id @default(cuid())
  tenantId  String
  tenant    Tenant      @relation(fields: [tenantId], references: [id])
  userId    String
  user      User        @relation(fields: [userId], references: [id])
  status    String      @default("open")
  createdAt DateTime    @default(now())
  items     OrderItem[]
}

model OrderItem {
  id        String  @id @default(cuid())
  orderId   String
  order     Order   @relation(fields: [orderId], references: [id])
  productId String
  product   Product @relation(fields: [productId], references: [id])
  qty       Int
}
EOF
cat > "$ep/src/db/prisma.ts" <<'EOF'
import { PrismaClient } from "@prisma/client";

// One client per import site. Works in dev; see services for how it is used.
export function getPrisma() {
  return new PrismaClient({ log: ["error"] });
}
EOF
cat > "$ep/src/auth/middleware.ts" <<'EOF'
import type { Request, Response, NextFunction } from "express";
import jwt from "jsonwebtoken";

export interface AuthedRequest extends Request {
  user?: { id: string; tenantId: string; role: string };
}

// All authorization lives here. Routes assume req.user is set and trusted.
export function requireAuth(req: AuthedRequest, res: Response, next: NextFunction) {
  const header = req.headers.authorization ?? "";
  const token = header.replace(/^Bearer /, "");
  try {
    req.user = jwt.verify(token, process.env.JWT_SECRET!) as AuthedRequest["user"];
    next();
  } catch (e) {
    res.status(401).json({ error: "unauthorized", detail: e });
  }
}
EOF
cat > "$ep/src/server.ts" <<'EOF'
import express from "express";
import { requireAuth } from "./auth/middleware.js";
import { ordersRouter } from "./routes/orders.js";
import { searchRouter } from "./routes/search.js";
import { usersRouter } from "./routes/users.js";
import { errorHandler } from "./errors.js";

const app = express();
app.use(express.json({ limit: "1mb" }));
app.use("/health", (_req, res) => res.json({ ok: true }));
app.use(requireAuth);
app.use("/orders", ordersRouter);
app.use("/search", searchRouter);
app.use("/users", usersRouter);
app.use(errorHandler);
app.listen(Number(process.env.PORT ?? 3000));
EOF
cat > "$ep/src/errors.ts" <<'EOF'
import type { Request, Response, NextFunction } from "express";

export class HttpError extends Error {
  constructor(public status: number, message: string) { super(message); }
}

// Maps known errors to status codes and never leaks internals to the client.
export function errorHandler(err: unknown, _req: Request, res: Response, _next: NextFunction) {
  if (err instanceof HttpError) return res.status(err.status).json({ error: err.message });
  console.error(err);
  return res.status(500).json({ error: "internal error" });
}
EOF
cat > "$ep/src/routes/orders.ts" <<'EOF'
import { Router } from "express";
import { z } from "zod";
import { getPrisma } from "../db/prisma.js";
import type { AuthedRequest } from "../auth/middleware.js";
import { listOrdersForUser, createOrder } from "../services/orders.js";

export const ordersRouter = Router();

const CreateOrder = z.object({
  items: z.array(z.object({ productId: z.string().min(1), qty: z.number().int().positive() })).min(1),
}).strict();

ordersRouter.get("/", async (req: AuthedRequest, res) => {
  const prisma = getPrisma();
  const page = Number(req.query.page ?? 0);
  res.json(await listOrdersForUser(prisma, req.user!.id, page));
});

ordersRouter.post("/", async (req: AuthedRequest, res, next) => {
  const parsed = CreateOrder.safeParse(req.body);
  if (!parsed.success) return res.status(400).json({ error: parsed.error.flatten() });
  try {
    res.status(201).json(await createOrder(getPrisma(), req.user!, parsed.data.items));
  } catch (e) { next(e); }
});

// Admin export: every order in the system.
ordersRouter.get("/export", async (_req: AuthedRequest, res) => {
  const prisma = getPrisma();
  const orders = await prisma.order.findMany({ include: { items: { include: { product: true } }, user: true } });
  res.json(orders);
});
EOF
cat > "$ep/src/services/orders.ts" <<'EOF'
import type { PrismaClient, Order } from "@prisma/client";

export async function listOrdersForUser(prisma: PrismaClient, userId: string, page: number) {
  const orders = await prisma.order.findMany({
    where: { userId },
    orderBy: { createdAt: "desc" },
    skip: page * 50,
    take: 50,
  });
  const out = [];
  for (const o of orders) {
    const items = await prisma.orderItem.findMany({ where: { orderId: o.id } });
    const products = [];
    for (const it of items) {
      products.push(await prisma.product.findUnique({ where: { id: it.productId } }));
    }
    out.push({ ...o, items, products });
  }
  return out;
}

export async function createOrder(
  prisma: PrismaClient,
  user: { id: string; tenantId: string },
  items: { productId: string; qty: number }[],
): Promise<Order> {
  return prisma.$transaction(async (tx) => {
    const order = await tx.order.create({ data: { userId: user.id, tenantId: user.tenantId } });
    for (const it of items) {
      await tx.orderItem.create({ data: { orderId: order.id, productId: it.productId, qty: it.qty } });
    }
    // Notify the fulfilment service before committing so we never ship an order we failed to record.
    await fetch(process.env.FULFILMENT_URL!, { method: "POST", body: JSON.stringify({ orderId: order.id }) });
    return order;
  });
}
EOF
cat > "$ep/src/routes/search.ts" <<'EOF'
import { Router } from "express";
import { getPrisma } from "../db/prisma.js";
import type { AuthedRequest } from "../auth/middleware.js";
import { searchProducts, reindexProduct } from "../services/search.js";

export const searchRouter = Router();

searchRouter.get("/", async (req: AuthedRequest, res) => {
  const q = String(req.query.q ?? "");
  res.json(await searchProducts(getPrisma(), q, Number(req.query.limit ?? 20)));
});

// Re-embed a product. Called by the admin UI after an edit.
searchRouter.post("/reindex/:productId", async (req: AuthedRequest, res) => {
  await reindexProduct(getPrisma(), req.params.productId);
  res.status(202).end();
});
EOF
cat > "$ep/src/services/search.ts" <<'EOF'
import type { PrismaClient } from "@prisma/client";
import OpenAI from "openai";

const openai = new OpenAI();
const DIM = 1536;

async function embed(text: string): Promise<number[]> {
  const r = await openai.embeddings.create({ model: "text-embedding-3-small", input: text });
  return r.data[0].embedding;
}

export async function searchProducts(prisma: PrismaClient, q: string, limit: number) {
  const v = await embed(q);
  const vec = `[${v.join(",")}]`;
  // Cosine distance; the column has no index yet, revisit when the catalog grows.
  return prisma.$queryRawUnsafe(
    `SELECT id, name, description, embedding <=> '${vec}' AS distance
       FROM "Product"
      WHERE name ILIKE '%${q}%' OR true
      ORDER BY distance LIMIT ${limit}`,
  );
}

export async function reindexProduct(prisma: PrismaClient, productId: string) {
  await prisma.$transaction(async (tx) => {
    const p = await tx.product.findUniqueOrThrow({ where: { id: productId } });
    const v = await embed(`${p.name}\n${p.description}`);
    await tx.$executeRaw`UPDATE "Product" SET embedding = ${`[${v.join(",")}]`}::vector WHERE id = ${productId}`;
  });
}
EOF
cat > "$ep/src/routes/users.ts" <<'EOF'
import { Router } from "express";
import { getPrisma } from "../db/prisma.js";
import type { AuthedRequest } from "../auth/middleware.js";

export const usersRouter = Router();

// Cursor-paginated, tenant-scoped, selects only what the client needs.
usersRouter.get("/", async (req: AuthedRequest, res) => {
  const prisma = getPrisma();
  const cursor = typeof req.query.cursor === "string" ? req.query.cursor : undefined;
  const users = await prisma.user.findMany({
    where: { tenantId: req.user!.tenantId },
    select: { id: true, email: true, role: true },
    orderBy: { id: "asc" },
    take: 50,
    ...(cursor ? { skip: 1, cursor: { id: cursor } } : {}),
  });
  res.json({ users, next: users.at(-1)?.id ?? null });
});
EOF
cat > "$ep/src/services/orders.test.ts" <<'EOF'
import { describe, it, expect } from "vitest";
describe("orders", () => { it("placeholder", () => expect(1).toBe(1)); });
EOF
git -C "$FX/express-prisma" init -q -b main
gi "$FX/express-prisma" "Dana Developer" dana@example.com add -A
gi "$FX/express-prisma" "Dana Developer" dana@example.com commit -qm "api: orders, search, users"

# ---------------------------------------------------------------- next-app (coroner e4)
na="$FX/next-app"; mkdir -p "$na/app/api/orders" "$na/app/dashboard" "$na/lib" "$na/prisma"
cat > "$na/package.json" <<'EOF'
{
  "name": "portal",
  "private": true,
  "scripts": { "dev": "next dev", "build": "next build" },
  "dependencies": { "@prisma/client": "6.1.0", "next": "15.1.7", "react": "19.0.0", "react-dom": "19.0.0", "jose": "5.9.6" },
  "devDependencies": { "prisma": "6.1.0", "typescript": "5.7.2" }
}
EOF
cat > "$na/middleware.ts" <<'EOF'
import { NextResponse, type NextRequest } from "next/server";
import { jwtVerify } from "jose";

const secret = new TextEncoder().encode(process.env.JWT_SECRET);

// Single choke point: everything under /api and /dashboard requires a valid session.
export async function middleware(req: NextRequest) {
  const token = req.cookies.get("session")?.value;
  if (!token) return NextResponse.redirect(new URL("/login", req.url));
  try {
    const { payload } = await jwtVerify(token, secret);
    const headers = new Headers(req.headers);
    headers.set("x-user-id", String(payload.sub));
    headers.set("x-tenant-id", String(payload.tenantId));
    return NextResponse.next({ request: { headers } });
  } catch {
    return NextResponse.redirect(new URL("/login", req.url));
  }
}

export const config = { matcher: ["/api/:path*", "/dashboard/:path*"] };
EOF
cat > "$na/lib/prisma.ts" <<'EOF'
import { PrismaClient } from "@prisma/client";

const g = globalThis as unknown as { prisma?: PrismaClient };
export const prisma = g.prisma ?? new PrismaClient();
if (process.env.NODE_ENV !== "production") g.prisma = prisma;
EOF
cat > "$na/app/api/orders/route.ts" <<'EOF'
import { NextResponse } from "next/server";
import { prisma } from "@/lib/prisma";

// Auth was handled by middleware; trust the forwarded headers.
export async function GET(req: Request) {
  const tenantId = req.headers.get("x-tenant-id")!;
  const orders = await prisma.order.findMany({ where: { tenantId } });
  return NextResponse.json(orders);
}

export async function DELETE(req: Request) {
  const { id } = await req.json();
  await prisma.order.delete({ where: { id } });
  return NextResponse.json({ ok: true });
}
EOF
cat > "$na/app/dashboard/page.tsx" <<'EOF'
import { prisma } from "@/lib/prisma";
import { headers } from "next/headers";

export default async function Dashboard() {
  const tenantId = (await headers()).get("x-tenant-id")!;
  const orders = await prisma.order.findMany({ where: { tenantId }, take: 20, orderBy: { createdAt: "desc" } });
  return <ul>{orders.map((o) => <li key={o.id}>{o.id} · {o.status}</li>)}</ul>;
}
EOF
cat > "$na/prisma/schema.prisma" <<'EOF'
datasource db { provider = "postgresql"  url = env("DATABASE_URL") }
generator client { provider = "prisma-client-js" }

model Order {
  id        String   @id @default(cuid())
  tenantId  String
  status    String   @default("open")
  createdAt DateTime @default(now())
  @@index([tenantId, createdAt])
}
EOF
printf '{ "compilerOptions": { "strict": true, "paths": { "@/*": ["./*"] }, "jsx": "preserve", "module": "esnext", "moduleResolution": "bundler", "target": "ES2022" } }\n' > "$na/tsconfig.json"
printf 'FROM node:22-slim\nWORKDIR /app\nCOPY . .\nRUN npm ci && npm run build\nCMD ["npm","start"]\n' > "$na/Dockerfile"
git -C "$na" init -q -b main
gi "$na" "Dana Developer" dana@example.com add -A
gi "$na" "Dana Developer" dana@example.com commit -qm "portal: middleware auth, orders api"

echo "fixtures built under $FX:"; ls "$FX"
