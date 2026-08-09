import { expect, test } from "@playwright/test";

const token = process.env.FLUVORA_BROWSER_TOKEN;
const secondToken = process.env.FLUVORA_BROWSER_TOKEN_2;
if (!token || !secondToken) {
  throw new Error("FLUVORA_BROWSER_TOKEN and FLUVORA_BROWSER_TOKEN_2 are required");
}

test("opens a reliable DataChannel against the native Rust SFU", async ({ page }) => {
  const fragment = new URLSearchParams({
    api: process.env.FLUVORA_BROWSER_API_URL ?? "http://127.0.0.1:18080",
    token,
  });
  await page.goto(`/tests/browser/#${fragment}`);

  const status = page.getByTestId("status");
  await expect(status).toHaveAttribute("data-result", "pass", { timeout: 30_000 });
  const result = await page.evaluate(() => globalThis.__fluvoraInterop);

  expect(result.status).toBe("PASS");
  expect(result.connectionState).toBe("connected");
  expect(result.iceConnectionState).toMatch(/^(connected|completed)$/u);
  expect(result.sctpState).toBe("connected");
  expect(result.dataChannel).toEqual({
    label: "fluvora.room.v1",
    protocol: "fluvora.v1",
    ordered: true,
    readyState: "open",
  });
  expect(result.partialDataChannel).toEqual({
    label: "fluvora.partial.v1",
    protocol: "fluvora.partial.v1",
    ordered: false,
    maxRetransmits: 0,
    readyState: "open",
  });
});

test("forwards browser VP8 video through the native Rust SFU", async ({ page }) => {
  const fragment = new URLSearchParams({
    api: process.env.FLUVORA_BROWSER_API_URL ?? "http://127.0.0.1:18080",
    token,
    token2: secondToken,
  });
  await page.goto(`/tests/browser/media.html#${fragment}`);

  const status = page.getByTestId("status");
  await expect(status).toHaveAttribute("data-result", "pass", { timeout: 40_000 });
  const result = await page.evaluate(() => globalThis.__fluvoraMediaInterop);

  expect(result.status).toBe("PASS");
  expect(result.subscriptionPath).toBe("direct");
  expect(result.remoteTrackState).toBe("live");
  expect(result.inbound.packetsReceived).toBeGreaterThanOrEqual(5);
  expect(result.inbound.bytesReceived).toBeGreaterThanOrEqual(1_000);
});

test("connects two browser peers through Fluvora P2P signaling", async ({ page }) => {
  const fragment = new URLSearchParams({
    api: process.env.FLUVORA_BROWSER_API_URL ?? "http://127.0.0.1:18080",
    token,
    token2: secondToken,
  });
  await page.goto(`/tests/browser/p2p.html#${fragment}`);

  const status = page.getByTestId("status");
  await expect(status).toHaveAttribute("data-result", "pass", { timeout: 35_000 });
  const result = await page.evaluate(() => globalThis.__fluvoraP2pInterop);

  expect(result.status).toBe("PASS");
  expect(result.peerA.connectionState).toBe("connected");
  expect(result.peerB.connectionState).toBe("connected");
  expect(result.peerA.dataChannelState).toBe("open");
  expect(result.peerB.dataChannelState).toBe("open");
  expect(result.message).toBe("fluvora-p2p-probe");
  expect(result.remoteVideoTrackState).toBe("live");
  expect(result.inboundVideo.packetsReceived).toBeGreaterThanOrEqual(5);
});
