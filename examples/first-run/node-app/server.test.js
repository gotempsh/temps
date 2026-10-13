// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
const { test, expect } = require("bun:test");
const net = require("net");
const { probe } = require("./server");
test("an open TCP socket cannot pass an authenticated managed-service probe", async () => {
  const sockets = new Set();
  const server = net.createServer((socket) => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    for (const protocol of ["postgres", "redis"]) {
      const result = await probe(
        `${protocol}://fixture:fixture@127.0.0.1:${server.address().port}`,
      );
      expect(result).toEqual({ present: true, parsed: true, reachable: false });
    }
  } finally {
    for (const socket of sockets) socket.destroy();
    await new Promise((resolve) => server.close(resolve));
  }
}, 10000);
