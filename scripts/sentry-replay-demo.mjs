// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// A visible synthetic application for the pinned official browser SDK 11.8.0
// (sentry-javascript c6492ef78c7bfb97931e43075763ce84ee8fda7e).
// Run with Bun, passing the installed SDK entry point and a local test DSN.
// The temporary HTTP application closes when recording finishes.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import { mkdtemp, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

const [entry, dsn, scenario = "shop", viewport = "desktop"] =
  process.argv.slice(2);
assert.ok(entry && dsn, "Provide an SDK entry point and local test DSN");
assert.ok(["shop", "landing"].includes(scenario), "Unknown synthetic scenario");
assert.ok(["desktop", "mobile"].includes(viewport), "Unknown viewport");
assert.ok(
  ["localhost", "127.0.0.1"].includes(new URL(dsn).hostname),
  "This demo only sends synthetic data to a local test instance",
);
const require = createRequire(pathToFileURL(entry));
const { chromium } = await import(pathToFileURL(require.resolve("playwright")));

const shopHtml = `<!doctype html>
<!-- SPDX-FileCopyrightText: 2024-2026 Temps Contributors
SPDX-License-Identifier: MIT OR Apache-2.0 -->
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Sample shop · Synthetic replay</title><style>
*{box-sizing:border-box}body{margin:0;color:#203047;background:#f4f6fa;font:16px system-ui,sans-serif}
header{position:sticky;top:0;z-index:1;background:#fff;border-bottom:1px solid #dce2ec;padding:22px 40px;display:flex;align-items:center;justify-content:space-between}
.brand{font-weight:750;font-size:23px;letter-spacing:-.6px}.badge{font-size:12px;color:#52627a;background:#edf1f7;padding:6px 10px;border-radius:20px}
.layout{display:grid;grid-template-columns:210px 1fr;max-width:1280px;margin:auto;min-height:calc(100vh - 80px)}
nav{padding:32px 20px}nav button{display:block;width:100%;text-align:left;margin-bottom:10px;background:transparent;color:#536379}
nav button.active{background:#e4eaff;color:#354bb6}main{padding:32px 36px 60px;max-width:1040px}
h1{font-size:30px;margin:6px 0 8px;letter-spacing:-.7px}h2{margin:0 0 14px;font-size:19px}p{color:#5d6e84;line-height:1.6}.intro{margin:0 0 28px}
.grid{display:grid;grid-template-columns:repeat(3,1fr);gap:18px}.card,.panel{background:#fff;border:1px solid #e0e5ee;border-radius:14px;padding:22px}
.art{height:130px;border-radius:10px;margin-bottom:18px;display:grid;place-items:center;font-size:48px;background:#eaf0ff}.mint{background:#dff3eb}.sand{background:#faf0d9}
.price{font-size:22px;font-weight:650;color:#203047}.row{display:flex;align-items:center;justify-content:space-between;gap:16px}.muted{font-size:13px;color:#728097}
button{border:0;border-radius:8px;background:#4359c9;color:#fff;font:600 14px system-ui;padding:12px 16px;cursor:pointer}button:hover{filter:brightness(.95)}
.secondary{background:#edf1f8;color:#435475}.notice{margin-top:22px;background:#e6f5ed;border:1px solid #c3e7d3;color:#246945;border-radius:10px;padding:16px}
.panel{margin:20px 0}.panel p{margin:6px 0}.section{margin-top:36px}.total{font-size:24px;font-weight:700}label{display:block;font-size:14px;font-weight:600;margin:18px 0 8px}
input,select{width:100%;padding:13px;border:1px solid #cbd4e3;border-radius:8px;background:white;font:16px system-ui;color:#203047}
.form-grid{display:grid;grid-template-columns:1fr 1fr;gap:20px}.step{display:inline-block;color:#4359c9;font-size:12px;font-weight:700;letter-spacing:1.4px;margin-bottom:8px}
.activity{display:flex;gap:18px;padding:20px 0;border-bottom:1px solid #edf0f5}.dot{margin-top:7px;width:10px;height:10px;border-radius:50%;background:#30a278;flex-shrink:0}
footer{padding-top:28px;color:#7b899d;font-size:13px}a{color:#4359c9}
</style></head><body>
<header><span class="brand" data-synthetic-public>Sample shop</span><span class="badge" data-synthetic-public>SYNTHETIC REPLAY DEMO · NO REAL ORDERS</span><span id="cart-count" data-synthetic-public>Bag · 0 items</span></header>
<div class="layout"><nav data-synthetic-public><button id="nav-shop" class="active">Browse collection</button><button id="nav-checkout">Shopping bag</button><button id="nav-activity">Activity</button></nav><main id="view"></main></div>
<script>
const products=[['lamp','Desk lamp','A warm light for focused work.','38','◒',''],['notebook','Grid notebook','Room for your next good idea.','12','▤','mint'],['cup','Ceramic cup','A small pause in your day.','18','◡','sand'],['clock','Table clock','A simpler way to keep time.','26','◷','sand'],['plant','Planter','A little green for your desk.','22','♧',''],['tray','Desk tray','A home for the small things.','16','▱','mint']];
let bag=[],completed=false;
const view=document.querySelector('#view');
const cart=()=>{document.querySelector('#cart-count').textContent='Bag · '+bag.length+' items'};
function renderShop(){view.innerHTML='<span class="step" data-synthetic-public>THE EVERYDAY COLLECTION</span><h1 data-synthetic-public>Make room for the everyday.</h1><p class="intro" data-synthetic-public>Thoughtful objects for a calmer workspace. Browse, add to your bag, and try the sample checkout.</p><div class="grid" data-synthetic-public>'+products.map(([id,name,description,price,art,color])=>'<article class="card"><div class="art '+color+'">'+art+'</div><h2>'+name+'</h2><p>'+description+'</p><div class="row"><span class="price">$'+price+'</span><button id="add-'+id+'" data-add="'+id+'">Add to bag</button></div></article>').join('')+'</div><section class="section panel" data-synthetic-public><h2>Made for a slower pace</h2><p>Simple materials. Useful shapes. Everyday essentials chosen for a quiet, comfortable workspace.</p><p>This entire application is generated test content, recorded with the official Sentry browser SDK.</p></section><div id="shop-notice" role="status" data-synthetic-public></div><footer data-synthetic-public>Sample shop · Synthetic application · No payment or order service connected</footer>';
view.querySelectorAll('[data-add]').forEach(button=>button.onclick=()=>{bag.push(button.dataset.add);cart();button.textContent='Added to bag';document.querySelector('#shop-notice').className='notice';document.querySelector('#shop-notice').textContent='Added to your bag. Continue browsing or open your shopping bag.'});}
function renderCheckout(){const total=bag.reduce((sum,id)=>sum+Number(products.find(p=>p[0]===id)[3]),0);view.innerHTML='<span class="step" data-synthetic-public>SAMPLE CHECKOUT</span><h1 data-synthetic-public>Your shopping bag</h1><p class="intro" data-synthetic-public>A synthetic checkout. No payment will be collected.</p><section class="panel" data-synthetic-public>'+bag.map(id=>{const p=products.find(p=>p[0]===id);return '<div class="row"><p>'+p[1]+'</p><strong>$'+p[3]+'</strong></div>'}).join('')+'<hr><div class="row"><span>Total</span><span class="total">$'+total+'</span></div></section><form id="checkout"><h2 data-synthetic-public>Delivery details</h2><p class="muted" data-synthetic-public>Input values stay masked in the recording.</p><div class="form-grid"><div><label for="sample-name" data-synthetic-public>Name</label><input id="sample-name" autocomplete="off" required></div><div><label for="sample-email" data-synthetic-public>Email</label><input id="sample-email" type="email" autocomplete="off" required></div></div><label for="shipping" data-synthetic-public>Delivery option</label><select id="shipping"><option value="standard">Standard delivery · Free</option><option value="express">Express delivery · $5</option></select><div class="section"><button id="place-order" data-synthetic-public>Place sample order</button></div></form>';
document.querySelector('#checkout').onsubmit=event=>{event.preventDefault();completed=true;view.innerHTML='<span class="step" data-synthetic-public>ALL SET</span><h1 id="order-complete" data-synthetic-public>Sample order complete</h1><p class="intro" data-synthetic-public>Your synthetic checkout is finished. No real order was created.</p><div class="notice" data-synthetic-public>Two everyday essentials, ready for a calmer desk.</div><section class="panel" data-synthetic-public><h2>What happened in this session</h2><p>Browsed the collection, scrolled the page, added two items, opened the bag and submitted the sample form.</p></section><button id="show-activity" data-synthetic-public>View activity</button>';document.querySelector('#show-activity').onclick=()=>navigate('activity')};}
function renderActivity(){view.innerHTML='<span class="step" data-synthetic-public>SESSION ACTIVITY</span><h1 data-synthetic-public>A complete sample journey</h1><p class="intro" data-synthetic-public>Navigation, clicks, scrolling and form interactions in one recording.</p><section class="panel" data-synthetic-public>'+['Browsed the everyday collection','Added a desk lamp to the bag','Added a grid notebook to the bag','Selected a delivery option',completed?'Completed the sample checkout':'Checkout ready'].map(text=>'<div class="activity"><span class="dot"></span><div><strong>'+text+'</strong><p class="muted">Synthetic interaction</p></div></div>').join('')+'</section><div id="demo-finished" class="notice" data-synthetic-public>Replay demo complete. All content is synthetic.</div>';}
function navigate(route){history.pushState({},'', '/'+route);document.title='Sample shop · '+route;document.querySelectorAll('nav button').forEach(button=>button.classList.toggle('active',button.id==='nav-'+(route==='collection'?'shop':route)));window.scrollTo(0,0);({collection:renderShop,checkout:renderCheckout,activity:renderActivity})[route]()}
document.querySelector('#nav-shop').onclick=()=>navigate('collection');document.querySelector('#nav-checkout').onclick=()=>navigate('checkout');document.querySelector('#nav-activity').onclick=()=>navigate('activity');renderShop();
</script></body></html>`;

const html =
  scenario === "landing"
    ? await readFile(
        new URL("./fixtures/sentry-landing.html", import.meta.url),
        "utf8",
      )
    : shopHtml;
const app = createServer((request, response) => {
  if (
    new URL(request.url, "http://localhost").pathname === "/synthetic-work.json"
  ) {
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end('{"synthetic":true,"status":"ok"}');
    return;
  }
  response.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
  response.end(html);
});
await new Promise((resolve) => app.listen(0, "127.0.0.1", resolve));
const directory = await mkdtemp(join(tmpdir(), "temps-sentry-visible-replay-"));
let browser;
try {
  const wrapper = join(directory, "entry.mjs");
  await writeFile(
    wrapper,
    `// SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n// SPDX-License-Identifier: MIT OR Apache-2.0\nimport * as S from ${JSON.stringify(entry)};window.SyntheticSentry=S;`,
  );
  const build = await Bun.build({
    entrypoints: [wrapper],
    target: "browser",
    format: "iife",
  });
  assert.equal(build.success, true);
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({
    viewport:
      viewport === "mobile"
        ? { width: 390, height: 844 }
        : { width: 1280, height: 800 },
    isMobile: viewport === "mobile",
    hasTouch: viewport === "mobile",
  });
  const errors = [];
  page.on("pageerror", () => errors.push("Synthetic application page error"));
  await page.goto(
    `http://127.0.0.1:${app.address().port}/${scenario === "landing" ? "" : "collection"}`,
  );
  await page.addScriptTag({ content: await build.outputs[0].text() });
  await page.addScriptTag({
    content: await readFile(
      new URL("./fixtures/sentry-demo-telemetry.js", import.meta.url),
      "utf8",
    ),
  });
  await page.evaluate((dsn) => {
    const S = window.SyntheticSentry;
    window.syntheticDeliveries = [];
    S.init({
      dsn,
      enableLogs: true,
      tracesSampleRate: 1,
      defaultIntegrations: false,
      sendClientReports: false,
      dataCollection: { userInfo: false },
      release: "synthetic-replay-demo-1",
      environment: "synthetic-test",
      replaysSessionSampleRate: 1,
      replaysOnErrorSampleRate: 0,
      integrations: [
        S.replayIntegration({
          maskAllText: true,
          maskAllInputs: true,
          // Only generic fixture labels are public. Never unmask form values.
          unmask: ["[data-synthetic-public]"],
          minReplayDuration: 0,
          flushMinDelay: 1000,
          flushMaxDelay: 5000,
        }),
      ],
      transport(options) {
        const native = S.makeFetchTransport(options);
        return {
          async send(envelope) {
            const response = await native.send(envelope);
            window.syntheticDeliveries.push(response.statusCode);
            return response;
          },
          flush: (timeout) => native.flush(timeout),
        };
      },
    });
    window.installSyntheticTelemetry(S);
  }, dsn);
  const started = Date.now();
  const pause = () => page.waitForTimeout(2500);
  if (scenario === "landing") {
    assert.equal(
      await page.evaluate(
        () =>
          document.documentElement.scrollWidth >
          document.documentElement.clientWidth,
      ),
      false,
      "The landing page overflows its initial viewport",
    );
    const step = async (selector, action = "click") => {
      await page.locator(selector).scrollIntoViewIfNeeded();
      await page.waitForTimeout(700);
      if (action === "click") await page.locator(selector).click();
      await page.waitForTimeout(1800);
      assert.equal(
        await page.evaluate(
          () =>
            document.documentElement.scrollWidth >
            document.documentElement.clientWidth,
        ),
        false,
        `The landing page overflowed at ${selector}`,
      );
    };
    await pause();
    await step("#explore");
    await step("#tab-create");
    assert.equal(
      await page.locator("#product-panel h3").textContent(),
      "Good ideas need a little room.",
    );
    await step("#tab-ship");
    await step("#tab-plan");
    await step("#billing-yearly");
    assert.equal(await page.locator("#team-price").textContent(), "$14");
    await step("#seats", "view");
    await page.locator("#seats").focus();
    for (let i = 0; i < 7; i++) await page.keyboard.press("ArrowRight");
    assert.equal(await page.locator("#estimate").textContent(), "$168");
    await pause();
    await page.evaluate(() => window.SyntheticSentry.getReplay().flush());
    await step("#faq-trial summary");
    await step("#faq-tools summary");
    await step("#faq-privacy summary");
    await step("#choose-team");
    await step("#continue-signup");
    assert.equal(await page.locator("#form-error").isVisible(), true);
    await step("#close-signup");
    await step("#choose-team");
    await page
      .locator("#full-name")
      .pressSequentially("Synthetic Visitor", { delay: 90 });
    await pause();
    await page
      .locator("#email")
      .pressSequentially("visitor@example.invalid", { delay: 70 });
    await page.locator("#team-size").selectOption("medium");
    await pause();
    await step("#continue-signup");
    await step("#template-creative");
    await page.evaluate(() => window.SyntheticSentry.getReplay().flush());
    await step("#create-workspace");
    await page.locator("#workspace-ready").waitFor();
    assert.equal(new URL(page.url()).pathname, "/welcome");
    await step("#check-project");
    await step("#check-plan");
    await step("#check-review");
    await page.locator("#landing-finished").waitFor();
    assert.equal(
      await page.locator("#onboarding-progress").textContent(),
      "3 of 3 steps complete",
    );
    await pause();
    assert.equal(
      await page.evaluate(
        () =>
          document.documentElement.scrollWidth >
          document.documentElement.clientWidth,
      ),
      false,
      "The fixture overflows its viewport",
    );
  } else {
    await pause();
    await page.mouse.move(560, 410, { steps: 20 });
    await pause();
    await page.locator("#add-lamp").click();
    await pause();
    await page.locator("#add-notebook").click();
    await page.evaluate(() => window.SyntheticSentry.getReplay().flush());
    await pause();
    await page.mouse.wheel(0, 650);
    await pause();
    await page.mouse.wheel(0, -650);
    await pause();
    await page.locator("#nav-checkout").click();
    await pause();
    await page.locator("#sample-name").pressSequentially("Synthetic Person", {
      delay: 90,
    });
    await pause();
    await page
      .locator("#sample-email")
      .pressSequentially("sample@example.invalid", {
        delay: 70,
      });
    await page.evaluate(() => window.SyntheticSentry.getReplay().flush());
    await pause();
    await page.locator("#shipping").selectOption("express");
    await pause();
    await page.locator("#place-order").click();
    await page.locator("#order-complete").waitFor();
    await pause();
    await page.locator("#show-activity").click();
    await page.locator("#demo-finished").waitFor();
    await pause();
  }
  const report = await page.evaluate(async () => {
    const S = window.SyntheticSentry;
    const replay = S.getReplay();
    const replayId = replay.getReplayId();
    window.SyntheticTelemetry.stop();
    await replay.flush();
    await replay.stop();
    await S.close(10000);
    return {
      sdk: S.SDK_VERSION,
      replay_id: replayId,
      deliveries: window.syntheticDeliveries,
    };
  });
  assert.equal(report.sdk, "11.8.0");
  assert.ok(report.replay_id);
  assert.ok(report.deliveries.length >= 3);
  assert.ok(report.deliveries.every((status) => status === 200));
  assert.deepEqual(errors, []);
  assert.ok(Date.now() - started >= 30000);
  process.stdout.write(
    JSON.stringify({
      ...report,
      scenario,
      viewport,
      duration_ms: Date.now() - started,
      journey:
        scenario === "landing"
          ? [
              "landing",
              "product_tabs",
              "pricing",
              "faq",
              "validation",
              "signup",
              "welcome",
              "onboarding",
            ]
          : ["collection", "checkout", "activity"],
    }) + "\n",
  );
} finally {
  if (browser) await browser.close();
  await new Promise((resolve) => app.close(resolve));
  await rm(directory, { recursive: true });
}
