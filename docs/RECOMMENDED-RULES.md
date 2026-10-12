# 推荐的分流规则

这是一份域名分组建议，不随程序内置。程序不预设任何域名，也不预设任何上游。

> 当前版本的设置页还不能逐条填规则：分流模式（`split-v1`）只把「目标站点」送进固定出口，其余交给兜底上游。
> 按下面的分组逐条分流，要等 [ADR-0008](adr/0008-routing-as-data.md) 落地；在那之前，这份清单可以用来决定
> 「目标站点」里放哪些域名，或者抄进你自己的 mihomo/Clash 配置。

判断一个域名该走固定出口还是走日常出口，只看一件事：**它带不带你的账号身份。**

- 带身份的（登录态、会话、账号画像、授权后端）→ 固定出口。同一个账号一次会话里出现两个国家，
  是最不该出现的信号。
- 不带身份的（遥测上报、静态资源 CDN、大流量分发）→ 日常出口或直连。它们数量大、频次高，
  留在付费住宅出口上只花钱、拖慢，还会把出口的抖动放大成「整个浏览器有时进得去有时进不去」。

## 三个顺序约束

规则首匹配即生效，所以下面三类必须排在宽泛规则之前：

1. **精确名写在后缀之前**。`DOMAIN,cloudflare.com` 和 `DOMAIN-SUFFIX,cloudflare.com` 是两个结果：
   后者会把 `cdnjs.cloudflare.com` 这种海量静态资源一起拖进付费出口。
2. **出口自检站跟随账号出口**。`ip.sb`、`ipinfo.io` 这类站如果落到日常出口，
   你「用固定出口的浏览器查自己的出口 IP」量到的会是另一条出口——这件事本身就是错的。
3. **Cloudflare 挑战与主站同出口**。质询用 `DOMAIN-SUFFIX,challenges.cloudflare.com`，
   因为 Turnstile 会走子域；用精确匹配会漏。同一会话里出现两个出口，challenge 的判定会飘。

## 建议分组

### 1. 对话与编码助手（走固定出口）

```
claude.ai  claude.com  claudeusercontent.com  anthropic.com
chatgpt.com  openai.com  oaistatic.com  oaiusercontent.com  sora.com
x.ai  grok.com
```

### 2. 搜索与账号域（走固定出口）

```
google.com  googleapis.com  gmail.com
```

静态资源与连通性探测不跟着走，留在日常出口：

```
gstatic.com  googleusercontent.com  gvt1.com
optimizationguide-pa.googleapis.com  play.googleapis.com
```

### 3. Microsoft 账号与商店

走固定出口：`microsoft.com`、`microsoftonline.com`、`live.com`、`xboxservices.com`、
`delivery.mp.microsoft.com`。后者是商店分发的边缘，国内合作 CDN 上常见未配置的 TLS 域名。

留在日常出口（大流量分发与纯遥测）：

```
mp.microsoft.com  do.dsp.mp.microsoft.com
functional.events.data.microsoft.com  ratingsedge.rnr.microsoft.com
smartscreen.microsoft.com  edge-http.microsoft.com
rewardsplatform.microsoft.com  edgeoffer.microsoft.com
microsoftedge.microsoft.com  edge.microsoft.com  uhf.microsoft.com
wcpstatic.microsoft.com  browser.pipe.aria.microsoft.com
```

遥测域一个域名可以在几分钟里产生上百次连接，每次都是新建连接。它们不带任何账号身份，
国内直连的端点自己就回 4xx，放在出口上纯粹是把出口的丢包窗口放大成浏览器打不开。

注意 `live.com` 与 `microsoftonline.com` 同时挂着登录后端：如果你把 `outlook.com` 之类的域
改成直连，要清楚同一账号的登录会和收件箱劈成两条出口。

### 4. X / Grok

走固定出口：`x.com`、`twitter.com`、`t.co`。
留在日常出口：`twimg.com`、`twitterusercontent.com`。媒体 CDN 是不同域，不携带 `x.com` 的登录态。

### 5. 账号身份域名（最容易漏的一组）

```
gmail.com  onedrive.com  office.com  office.net  office365.com
```

漏掉这些域名的结果是同一账号在同一浏览器里被劈成两条出口：主域走固定出口，
它的收件箱后端走日常出口。

### 6. 出口自检与信誉查询站（走固定出口）

```
ip.sb  ipify.org  ip.me  ipinfo.io  ip-api.com  ipquery.io  ipapi.is
scamalytics.com  browserleaks.com  whoer.net  ipleak.net  ping0.cc  dnsleaktest.com
```

### 7. Cloudflare

```
DOMAIN,cloudflare.com            → 固定出口
DOMAIN-SUFFIX,challenges.cloudflare.com  → 固定出口
```

### 8. 开发与包管理（走日常出口）

```
github.com  githubusercontent.com  githubassets.com  githubcopilot.com
```

机房出口对 GitHub 通常更快，而且一个域能在几分钟里占掉上百次失败重试，是最大的单一来源。
整组一起归位，避免同一账号的请求被劈到两条出口。

### 9. 兜底

最后一条 `MATCH` 指向你的默认路径。你有日常代理就填它，没有就填 `DIRECT`。
注意写成 `DIRECT` 时，目标站点之外的境外域名会直连并失败——这是如实失败，不是降级。

程序按你填的内容生成，不替你猜。

## 一条不要做的事

不要把固定出口当成「所有流量的总出口」。它按流量计费，而且住宅出口的容量有限：
遥测、商店分发、媒体 CDN 全部压上去，最直接的后果不是账单，是每个抖动窗口里
浏览器都变成「有时打得开有时打不开」，而检测报告会说一切正常——因为链路确实活着。
