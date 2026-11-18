# 发版前必读

> **只在准备发布 Release 时读这份文件。不打算发版，可以直接忽略整个文件。**

---

## 为什么会有这份文件

2026-11-19 的依赖安全升级（commit `5c091be`）为了清掉 13 条 RustSec 公告，把自动更新链路的底层实现换掉了大半：

| 更新环节 | 用到的库 | 本次变化 |
|---|---|---|
| 拉 `latest.json` | reqwest | 0.12 → **0.13**（大版本，TLS 后端换成 `rustls-platform-verifier`） |
| 下载 `.nsis.zip` | reqwest | 同上 |
| 校验签名 | minisign-verify | 0.2 → 0.3 |
| 解包 zip | zip | 4 → **9**（6 条公告就是它） |
| 装完重启 | tauri | 2.9 → 2.12 |

**关键事实：这个项目至今没有发布过任何 Release。** 也就是说自动更新链路一次都没有真正跑过——`check()` 请求的 `releases/latest/download/latest.json` 从来没有存在过。所以这些改动目前**没有任何生产验证**，第一次发版就是第一次真跑。

已做过的冒烟（2026-11-19，临时测试，跑完已删）：

- ✅ 生产公钥合法（56 字符），且 `tauri.conf.json` 与 `src/commands/verify.rs` 里是同一把
- ✅ minisign-verify 0.3 完整验签通过，篡改数据被正确拒绝
- ✅ reqwest 0.13 能正常建立 HTTPS 连接

**没做过的**：真实的下载 → 解包 → 安装 → 重启全流程。这就是下面要做的事。

---

## 发布前必须做的验证

### 本地跑一遍完整更新流程（不发真 Release）

思路：用同一份代码分别构建「旧版」和「新版」，把更新端点指到本地 HTTP 服务。这样能完整覆盖 check → download → verify → extract → install → restart，又不惊动任何用户。

**1. 生成一对临时测试密钥**（不要用生产密钥，生产私钥只在 GitHub Secrets 里）

```powershell
npx tauri signer generate -w ./.tauri/test.key
# 会同时生成 .tauri/test.key.pub，内容就是要填进配置的 pubkey
```

**2. 建测试配置 `src-tauri/tauri.updatetest.conf.json`**

```json
{
  "plugins": {
    "updater": {
      "endpoints": ["http://127.0.0.1:8787/latest.json"],
      "pubkey": "<.tauri/test.key.pub 的全部内容>"
    }
  }
}
```

**3. 构建「旧版」并安装**（版本号保持当前值不动）

```powershell
npx tauri build --bundles nsis --config src-tauri/tauri.updatetest.conf.json
# 安装 src-tauri\target\release\bundle\nsis\ 下的 exe
```

**4. 版本号 +1，构建「新版」**

要改三处，必须一致：`package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json`

```powershell
npx tauri build --bundles nsis --config src-tauri/tauri.updatetest.conf.json
```

产物里会有 `LightC_<新版本>_x64-setup.nsis.zip` 和同名的 `.sig`。

**5. 手写 `latest.json`**，和 zip、sig 放同一目录

```json
{
  "version": "<新版本>",
  "notes": "updater smoke test",
  "pub_date": "2026-11-19T00:00:00Z",
  "platforms": {
    "windows-x86_64": {
      "signature": "<.nsis.zip.sig 文件的全部内容>",
      "url": "http://127.0.0.1:8787/LightC_<新版本>_x64-setup.nsis.zip"
    }
  }
}
```

**6. 起本地服务并触发更新**

```powershell
python -m http.server 8787
```

启动已安装的「旧版」，等 2 秒（installer 渠道会自动检查更新），或手动点检查更新。

**通过标准**

- [ ] 能识别出新版本号
- [ ] 下载不报错
- [ ] 签名校验通过（不是「签名不一致」）
- [ ] 应用自己退出，NSIS 静默装完
- [ ] 重新启动后版本号是新的
- [ ] `%LOCALAPPDATA%\LightC\` 里的用户数据还在

**反向验证（建议做）**：把 zip 改掉一个字节再试，应该报签名校验失败、不执行安装。

**7. 收尾**：删掉 `tauri.updatetest.conf.json` 和 `.tauri/test.key*`，版本号改回你要发布的正式值。

---

## 发布时的硬性约束

这些是 `.github/workflows/release.yml` 的规矩，踩了会直接失败：

- 触发方式：推 `v*` 标签，例如 `git tag v2.20.0 && git push origin v2.20.0`
- 版本号来源：`CHANGELOG-zh.md` 里**第一个** `## vX.Y.Z` 段落，**必须和 tag 完全一致**，否则 workflow 报错退出
- 签名密钥：GitHub Secrets 的 `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。本地只有 `.tauri/lightc.key.pub`（公钥），**没有私钥**，所以本地无法签正式包
- ⚠️ `releaseDraft: false` + `prerelease: false`：**一推标签就是正式发布，所有用户立刻能收到更新**。想先灰度需要先改 workflow
- `createUpdaterArtifacts: true` 已在 `tauri.conf.json` 里开着，会自动产出 `.nsis.zip` + `.sig` 和 `latest.json`

---

## 检查清单

- [ ] 本地完整更新流程跑通（上面第 1~6 步）
- [ ] 反向验证：篡改后的包被拒绝
- [ ] `CHANGELOG-zh.md` 顶部已加新版本段落，版本号和要打的 tag 一致
- [ ] `package.json` / `src-tauri/Cargo.toml` / `src-tauri/tauri.conf.json` 三处版本号一致
- [ ] `tauri.updatetest.conf.json`、`.tauri/test.key*` 已清理
- [ ] 确认 GitHub Secrets 里的签名私钥仍然可用（发版前可先用 `npx tauri signer sign` 本地验一次，如果有私钥）
- [ ] 决定是否要先灰度（需要改 `release.yml` 的 `prerelease` 开关）
