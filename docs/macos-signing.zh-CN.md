# macOS 发布签名

`release-ui.yml` 为 Apple Silicon 和 Intel 分别构建 DMG。打包前校验 FFI 与本次构建一致，再由 `scripts/release/macos_package.py` 签名和公证：

1. 检查主程序、FFI 和 Flutter 框架的架构、动态库路径及最低系统版本。
2. 从内到外为原生二进制、框架和应用签署 Developer ID Application，启用 hardened runtime 和安全时间戳，保留 Release entitlements。
3. 应用 ZIP 通过 Apple 公证后，将票据装订到应用；生成 DMG，再签名、公证和装订 DMG。
4. 挂载最终 DMG，核对版本、签名团队、公证票据及 Gatekeeper 结果。全部通过才上传；失败不会发布临时签名或未公证的替代包。

安装包名称保持 `pb-mapper-macos-arm64.dmg`、`pb-mapper-macos-x86_64.dmg`，供现有更新清单使用。签名会改变文件内容，因此 FFI 哈希比较必须在签名前执行，发布清单必须在最终公证和装订后生成。这个流程处理桌面 UI 的 DMG；CLI 和 npm 的发布流程独立。

## Actions 配置

在 `acking-you/pb-mapper` 的 Actions 设置中配置以下项目。证书使用当前开发者账号的 Developer ID Application；公证使用该团队的 App Store Connect API key。

| 类型 | 名称 | 内容 |
| --- | --- | --- |
| Secret | `MACOS_CERTIFICATE_P12_BASE64` | 含证书、私钥和中间证书的 PKCS#12，Base64 编码 |
| Secret | `MACOS_CERTIFICATE_PASSWORD` | PKCS#12 密码 |
| Secret | `APPLE_NOTARY_PRIVATE_KEY` | 公证 API 私钥 |
| Variable | `MACOS_SIGNING_IDENTITY` | Developer ID Application 证书 SHA-1 或完整名称 |
| Variable | `APPLE_TEAM_ID` | 开发者团队 ID |
| Variable | `APPLE_NOTARY_KEY_ID` | 公证 API key ID |
| Variable | `APPLE_NOTARY_ISSUER_ID` | 公证 API issuer UUID |

每次作业使用临时钥匙串，完成或失败后清理。凭据不进入源码、构建产物或验证日志。公证被拒绝或等待超时时保留 submission ID；被拒绝时另保存 Apple 诊断日志。票据下载与校验遇到临时网络错误时，按 5、15、45 秒间隔最多重试三次；签名或票据本身的错误直接失败。

合并前或轮换凭据后，可在修复分支上手动运行 `release-ui.yml`，设置 `macos_verify_only=true`。这会验证两种架构并上传 Actions artifacts，不创建或修改 GitHub Release。验证日志位于 `macos-<arch>-verification` artifact。

本机可使用已有签名身份与公证 profile；钥匙串名称必须是该 profile 实际所在的钥匙串：

```sh
export MACOS_SIGNING_IDENTITY='Developer ID Application: NAME (TEAM)'
export APPLE_TEAM_ID='TEAM'
export APPLE_NOTARY_PROFILE='existing-profile'
export MACOS_SIGNING_KEYCHAIN='/path/to/signing.keychain-db'
export APPLE_NOTARY_KEYCHAIN='/path/to/notary.keychain-db'
make build-ui-macos-release
python3 scripts/release/macos_package.py \
  --app ui/build/macos/Build/Products/Release/pb-mapper.app \
  --arch arm64 --output ui/dist/pb-mapper-macos-arm64.dmg
```

脚本签署应用副本，不覆盖输入应用或已存在的输出文件。安装验证不能通过移除 quarantine 或关闭 Gatekeeper 来替代。

参考：[Apple 公证流程](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)、[GitHub Actions 证书配置](https://docs.github.com/en/actions/how-tos/deploy/deploy-to-third-party-platforms/sign-xcode-applications)。
