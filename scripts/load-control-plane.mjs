#!/usr/bin/env node
import { randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";
import process from "node:process";
import { performance } from "node:perf_hooks";

function parseArguments(values) {
  const configuration = {
    baseUrl: process.env.FLUVORA_LOAD_BASE_URL ?? "http://127.0.0.1:18080",
    token: process.env.FLUVORA_LOAD_TOKEN,
    tokenFile: process.env.FLUVORA_LOAD_TOKEN_FILE,
    tokenRefreshSeconds: 30,
    concurrency: 8,
    iterations: 10,
    durationSeconds: 0,
    maximumP95Millis: 1_000,
    maximumErrorRate: 0,
  };
  for (let index = 0; index < values.length; index += 1) {
    const argument = values[index];
    if (argument === "--profile") {
      const profile = values[++index];
      if (profile === "quick") Object.assign(configuration, { concurrency: 8, iterations: 10 });
      else if (profile === "capacity")
        Object.assign(configuration, { concurrency: 32, iterations: 100 });
      else if (profile === "soak")
        Object.assign(configuration, {
          concurrency: 16,
          iterations: Number.MAX_SAFE_INTEGER,
          durationSeconds: 172_800,
        });
      else throw new Error(`unsupported profile ${profile}`);
    } else if (argument === "--base-url") configuration.baseUrl = values[++index];
    else if (argument === "--token") configuration.token = values[++index];
    else if (argument === "--token-file") configuration.tokenFile = values[++index];
    else if (argument === "--token-refresh-seconds")
      configuration.tokenRefreshSeconds = parsePositiveInteger(values[++index], argument);
    else if (argument === "--concurrency")
      configuration.concurrency = parsePositiveInteger(values[++index], argument);
    else if (argument === "--iterations")
      configuration.iterations = parsePositiveInteger(values[++index], argument);
    else if (argument === "--duration-seconds")
      configuration.durationSeconds = parsePositiveInteger(values[++index], argument);
    else if (argument === "--maximum-p95-ms")
      configuration.maximumP95Millis = parsePositiveNumber(values[++index], argument);
    else if (argument === "--maximum-error-rate")
      configuration.maximumErrorRate = parseNonNegativeNumber(values[++index], argument);
    else if (argument === "--help") {
      throw new Error(
        "usage: load-control-plane.mjs [--profile quick|capacity|soak] " +
          "[--base-url URL] [--token TOKEN] [--concurrency N] [--iterations N] " +
          "[--token-file PATH] [--token-refresh-seconds N] [--duration-seconds N] " +
          "[--maximum-p95-ms N] [--maximum-error-rate N]",
      );
    } else throw new Error(`unsupported argument ${argument}`);
  }
  configuration.baseUrl = configuration.baseUrl.replace(/\/+$/u, "");
  if (!/^https?:\/\//u.test(configuration.baseUrl)) throw new Error("base URL must be HTTP(S)");
  if (!configuration.token && !configuration.tokenFile) {
    throw new Error(
      "FLUVORA_LOAD_TOKEN/--token or FLUVORA_LOAD_TOKEN_FILE/--token-file is required",
    );
  }
  if (configuration.token && configuration.tokenFile) {
    throw new Error("configure either a static token or a token file, not both");
  }
  if (configuration.concurrency > 1_024 || configuration.iterations > 10_000_000) {
    throw new Error("load configuration exceeds safety bounds");
  }
  if (configuration.maximumErrorRate > 1) {
    throw new Error("maximum error rate must be between zero and one");
  }
  return configuration;
}

function parsePositiveInteger(value, option) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) throw new Error(`${option} must be positive`);
  return parsed;
}

function parsePositiveNumber(value, option) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed <= 0) throw new Error(`${option} must be positive`);
  return parsed;
}

function parseNonNegativeNumber(value, option) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < 0) throw new Error(`${option} must be non-negative`);
  return parsed;
}

function identifier() {
  return randomBytes(16).toString("hex");
}

function percentile(sorted, quantile) {
  if (sorted.length === 0) return 0;
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * quantile) - 1)];
}

async function main() {
  const configuration = parseArguments(process.argv.slice(2));
  const latencies = [];
  const errors = [];
  let requestCount = 0;
  let completedFlows = 0;
  const started = performance.now();
  const deadline =
    configuration.durationSeconds > 0
      ? started + configuration.durationSeconds * 1_000
      : Number.POSITIVE_INFINITY;
  let cachedToken = configuration.token;
  let tokenReadAt = Number.NEGATIVE_INFINITY;
  let tokenRefresh;

  async function resolveToken(force = false) {
    if (!configuration.tokenFile) return configuration.token;
    const now = performance.now();
    if (
      !force &&
      cachedToken &&
      now - tokenReadAt < configuration.tokenRefreshSeconds * 1_000
    ) {
      return cachedToken;
    }
    if (!tokenRefresh) {
      tokenRefresh = readFile(configuration.tokenFile, "utf8")
        .then((value) => {
          const token = value.trim();
          if (!token) throw new Error(`token file ${configuration.tokenFile} is empty`);
          cachedToken = token;
          tokenReadAt = performance.now();
          return token;
        })
        .finally(() => {
          tokenRefresh = undefined;
        });
    }
    return tokenRefresh;
  }

  async function request(path, options = {}) {
    const requestStarted = performance.now();
    let response;
    for (let attempt = 0; attempt < 2; attempt += 1) {
      requestCount += 1;
      const headers = new Headers({
        Authorization: `Bearer ${await resolveToken(attempt > 0)}`,
        "Content-Type": "application/json",
      });
      if (options.idempotent) headers.set("Idempotency-Key", identifier());
      response = await fetch(`${configuration.baseUrl}${path}`, {
        method: options.method ?? "POST",
        headers,
        body: options.body === undefined ? undefined : JSON.stringify(options.body),
      });
      if (response.status !== 401 || !configuration.tokenFile || attempt > 0) break;
      await response.body?.cancel();
    }
    latencies.push(performance.now() - requestStarted);
    if (!response.ok) {
      const responseBody = await response.text();
      throw new Error(`${options.method ?? "POST"} ${path}: ${response.status} ${responseBody}`);
    }
    if (response.status === 204) return undefined;
    return response.json();
  }

  async function flow(worker, iteration) {
    let roomId;
    try {
      const room = await request("/v1/rooms", {
        idempotent: true,
        body: { mode: "p2p", max_members: 4, max_publishers: 2 },
      });
      roomId = room.room_id;
      await request(`/v1/rooms/${roomId}/chat`, {
        idempotent: true,
        body: {
          message_id: identifier(),
          text: `load-${worker}-${iteration}`,
        },
      });
      await request(`/v1/rooms/${roomId}/custom`, {
        idempotent: true,
        body: {
          namespace: "fluvora.load.v1",
          schema_version: 1,
          payload: { worker, iteration },
        },
      });
      await request(`/v1/rooms/${roomId}/signals`, {
        idempotent: true,
        body: {
          kind: "renegotiate",
          payload: { probe: true, worker, iteration },
        },
      });
      await request(`/v1/rooms/${roomId}/end`, { idempotent: true, body: {} });
      completedFlows += 1;
    } catch (error) {
      errors.push(error instanceof Error ? error.message : String(error));
      if (roomId) {
        await request(`/v1/rooms/${roomId}/end`, { idempotent: true, body: {} }).catch(() => {});
      }
    }
  }

  await Promise.all(
    Array.from({ length: configuration.concurrency }, async (_, worker) => {
      for (let iteration = 0; iteration < configuration.iterations; iteration += 1) {
        if (performance.now() >= deadline) break;
        await flow(worker, iteration);
      }
    }),
  );

  const elapsedMillis = performance.now() - started;
  latencies.sort((left, right) => left - right);
  const errorRate = errors.length / Math.max(1, completedFlows + errors.length);
  const p95Millis = percentile(latencies, 0.95);
  const passed =
    errorRate <= configuration.maximumErrorRate &&
    p95Millis <= configuration.maximumP95Millis &&
    completedFlows > 0;
  const result = {
    schema: "fluvora.perf.control.v1",
    concurrency: configuration.concurrency,
    configuredIterations: configuration.iterations,
    configuredDurationSeconds: configuration.durationSeconds,
    completedFlows,
    requests: requestCount,
    elapsedMillis: Math.round(elapsedMillis),
    requestsPerSecond: Math.round((requestCount * 1_000) / elapsedMillis),
    errorCount: errors.length,
    errorRate,
    latencyMillis: {
      p50: Math.round(percentile(latencies, 0.5) * 1_000) / 1_000,
      p95: Math.round(p95Millis * 1_000) / 1_000,
      p99: Math.round(percentile(latencies, 0.99) * 1_000) / 1_000,
    },
    thresholds: {
      maximumP95Millis: configuration.maximumP95Millis,
      maximumErrorRate: configuration.maximumErrorRate,
    },
    sampleErrors: errors.slice(0, 5),
    passed,
  };
  console.log(JSON.stringify(result));
  if (!passed) process.exitCode = 1;
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 2;
});
