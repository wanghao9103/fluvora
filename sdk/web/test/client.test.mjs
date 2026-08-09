import assert from "node:assert/strict";
import test from "node:test";

import { FluvoraClient, FluvoraResponseTooLargeError, SfuSession } from "../dist/index.js";

test("emits the v1 gift and media lifecycle contracts", async () => {
  const requests = [];
  const fetch = async (input, init) => {
    const url = new URL(String(input));
    requests.push({ url, init });
    if (url.pathname === "/v1/live/channel") {
      return Response.json({
        stream_id: "channel",
        next_sequence: 7,
        manifest_url: "https://media.example/channel/index.m3u8",
      });
    }
    return new Response(null, { status: 204 });
  };
  const client = new FluvoraClient({
    baseUrl: "https://api.example.com/",
    accessToken: "access-token",
    fetch,
  });
  const room = "00000000000000000000000000000001";
  await client.recordVerifiedGift(room, {
    provider: "payment-provider",
    providerTimestampMillis: 1_800_000_000_000,
    providerSignature: "base64url-signature",
    senderId: "00000000000000000000000000000002",
    recipientId: "00000000000000000000000000000003",
    transactionId: "transaction-42",
    giftId: "rocket",
    quantity: 2,
    unitValue: 500,
    currency: "CNY",
  });
  await client.deleteAsset("asset_1");
  const live = await client.getLiveOutput("channel");
  await client.deleteLiveOutput("channel");

  assert.equal(live.nextSequence, 7);
  assert.equal(requests[0].url.pathname, `/v1/rooms/${room}/gifts`);
  assert.deepEqual(JSON.parse(requests[0].init.body), {
    provider: "payment-provider",
    provider_timestamp_millis: 1_800_000_000_000,
    provider_signature: "base64url-signature",
    sender_id: "00000000000000000000000000000002",
    recipient_id: "00000000000000000000000000000003",
    transaction_id: "transaction-42",
    gift_id: "rocket",
    quantity: 2,
    unit_value: 500,
    currency: "CNY",
  });
  assert.equal(requests[1].init.method, "DELETE");
  assert.match(requests[1].init.headers.get("Idempotency-Key"), /^[0-9a-f]{32}$/u);
  assert.equal(requests[2].init.method, "GET");
  assert.equal(requests[3].init.method, "DELETE");
  for (const request of requests) {
    assert.equal(request.init.headers.get("Authorization"), "Bearer access-token");
    assert.equal(request.init.redirect, "error");
  }
});

test("rejects ambiguous base URLs and unsafe static or refreshed tokens", async () => {
  for (const baseUrl of [
    "file:///tmp/fluvora",
    "https://token@api.example.com",
    "https://api.example.com?redirect=true",
    "https://api.example.com#fragment",
    "https://",
  ]) {
    assert.throws(
      () => new FluvoraClient({ baseUrl, accessToken: "token", fetch: async () => Response.json({}) }),
      TypeError,
    );
  }
  assert.throws(
    () =>
      new FluvoraClient({
        baseUrl: "https://api.example.com",
        accessToken: "line\nbreak",
        fetch: async () => Response.json({}),
      }),
    TypeError,
  );

  let called = false;
  const client = new FluvoraClient({
    baseUrl: "https://api.example.com",
    accessToken: async () => "line\nbreak",
    fetch: async () => {
      called = true;
      return Response.json({});
    },
  });
  await assert.rejects(() => client.getRoom("01"), TypeError);
  assert.equal(called, false);
});

test("bounds both successful and error response bodies", async () => {
  for (const [status, contentLength] of [
    [200, 32 * 1_024 * 1_024 + 1],
    [500, 64 * 1_024 + 1],
  ]) {
    const client = new FluvoraClient({
      baseUrl: "https://api.example.com",
      accessToken: "token",
      fetch: async () =>
        new Response("{}", {
          status,
          headers: { "Content-Length": String(contentLength) },
        }),
    });
    await assert.rejects(
      () => client.getRoom("01"),
      (error) => error instanceof FluvoraResponseTooLargeError,
    );
  }
});

test("preserves a base URL path prefix and normalizes non-object errors", async () => {
  let requestedPath;
  let requestCount = 0;
  const client = new FluvoraClient({
    baseUrl: "https://api.example.com/control/",
    accessToken: "token",
    fetch: async (input) => {
      requestedPath = new URL(String(input)).pathname;
      requestCount += 1;
      return new Response(requestCount === 1 ? "null" : '{"code":17,"message":{}}', {
        status: 502,
      });
    },
  });
  for (let index = 0; index < 2; index += 1) {
    await assert.rejects(
      () => client.getRoom("01"),
      (error) =>
        error.code === "http_error" &&
        error.status === 502 &&
        error.message === "Fluvora request failed with 502",
    );
  }
  assert.equal(requestedPath, "/control/v1/rooms/01");
});

test("caps authoritative DataChannel envelopes at 16 KiB including the header", () => {
  let sent;
  const session = new SfuSession(
    "session",
    { close() {} },
    {},
    {
      label: "fluvora.room.v1",
      readyState: "open",
      send(value) {
        sent = value;
      },
    },
  );
  try {
    assert.throws(() => session.sendData("raw bypass"), /use sendRoomData/u);
    session.sendRoomData("chat", new Uint8Array(16 * 1_024 - 60));
    assert.equal(sent.byteLength, 16 * 1_024);
    assert.throws(
      () => session.sendRoomData("chat", new Uint8Array(16 * 1_024 - 59)),
      RangeError,
    );
  } finally {
    session.close();
  }
});

test("bounds raw messages on non-authoritative DataChannels", () => {
  const sent = [];
  const session = new SfuSession(
    "session",
    { close() {} },
    {},
    {
      label: "application.custom",
      readyState: "open",
      send(value) {
        sent.push(value);
      },
    },
  );
  try {
    session.sendData(new Uint8Array(16 * 1_024));
    assert.equal(sent.length, 1);
    assert.throws(() => session.sendData(new Uint8Array(16 * 1_024 + 1)), RangeError);
    assert.throws(() => session.sendData("x".repeat(16 * 1_024 + 1)), RangeError);
  } finally {
    session.close();
  }
});

test("rejects unsafe identifiers before issuing a request", async () => {
  let called = false;
  const client = new FluvoraClient({
    baseUrl: "https://api.example.com",
    accessToken: "access-token",
    fetch: async () => {
      called = true;
      return new Response(null, { status: 204 });
    },
  });
  await assert.rejects(() => client.deleteAsset("../escape"), TypeError);
  assert.equal(called, false);
});

test("rejects oversized control-plane payloads before issuing a request", async () => {
  let called = false;
  const client = new FluvoraClient({
    baseUrl: "https://api.example.com",
    accessToken: "access-token",
    fetch: async () => {
      called = true;
      return Response.json({});
    },
  });

  assert.throws(() => client.sendChat("01", "x".repeat(4_097)), RangeError);
  assert.throws(() => client.sendCustomData("01", ".invalid", 1, {}), TypeError);
  assert.throws(
    () => client.sendCustomData("01", "com.example.state", 1, "x".repeat(60 * 1_024)),
    RangeError,
  );
  await assert.rejects(
    client.postSignal("01", { kind: "offer", payload: "x".repeat(64 * 1_024) }),
    RangeError,
  );
  assert.equal(called, false);
});

test("rejects empty and oversized media uploads before issuing a request", async () => {
  let called = false;
  const client = new FluvoraClient({
    baseUrl: "https://api.example.com",
    accessToken: "access-token",
    fetch: async () => {
      called = true;
      return Response.json({});
    },
  });

  await assert.rejects(client.uploadLiveInit("stream", new Uint8Array()), TypeError);
  await assert.rejects(
    client.uploadAssetChunk("asset", 0, new Uint8Array(8 * 1_024 * 1_024 + 1)),
    RangeError,
  );
  await assert.rejects(
    client.uploadLiveSegment("stream", 0, 4_000, new Uint8Array(8 * 1_024 * 1_024 + 1)),
    RangeError,
  );
  assert.equal(called, false);
});

test("polls only the bounded signal page size", async () => {
  let requested;
  const client = new FluvoraClient({
    baseUrl: "https://api.example.com",
    accessToken: "access-token",
    fetch: async (input) => {
      requested = String(input);
      return Response.json({ signals: [], latest_sequence: 0 });
    },
  });
  await client.pollSignals("01", 0);
  assert.match(requested, /[?&]limit=128(?:&|$)/u);
});

test("emits a live ABR rendition ladder", async () => {
  let requestBody;
  const client = new FluvoraClient({
    baseUrl: "https://api.example.com",
    accessToken: "access-token",
    fetch: async (_input, init) => {
      requestBody = JSON.parse(init.body);
      return Response.json({
        stream_id: "live_abr",
        next_sequence: 0,
        manifest_url: "https://media.example/live_abr/master.m3u8",
      });
    },
  });
  const output = await client.createLiveAbrOutputFromTracks(
    "live_abr",
    [
      {
        roomId: "00000000000000000000000000000001",
        trackId: 7,
        kind: "video",
        codec: "vp8",
        payloadType: 96,
        clockRate: 90_000,
      },
    ],
    [
      {
        width: 640,
        height: 360,
        videoBitrateBps: 600_000,
        audioBitrateBps: 64_000,
      },
    ],
    { segmentDurationMillis: 1_000 },
  );
  assert.equal(output.manifestUrl, "https://media.example/live_abr/master.m3u8");
  assert.deepEqual(requestBody.renditions, [
    {
      width: 640,
      height: 360,
      video_bitrate_bps: 600_000,
      audio_bitrate_bps: 64_000,
    },
  ]);
  assert.equal(requestBody.source_tracks[0].track_id, 7);
});
