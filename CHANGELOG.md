# Changelog
All notable changes to this project will be documented in this file. See [conventional commits](https://www.conventionalcommits.org/) for commit guidelines.

- - -
## [v0.28.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/ed717999f4be4f380c9d93ee346040fe54e1a34d..v0.28.0) - 2026-09-11
#### Features
- (**diagnostics**) stream bounded redacted capture exports (#276) - ([d49492c](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/d49492c0a1feb7af849e6359ffc1d545cde8bc2b)) - Maciej Spiechowicz
- (**diagnostics**) retain bounded debug traces with explicit gaps (#275) - ([2ebd2ab](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/2ebd2aba74cc7aeebde5f901e4b75aa4d3b661eb)) - Maciej Spiechowicz
- (**diagnostics**) correlate bounded asynchronous flow events (#274) - ([96171fb](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/96171fb94d4152afb1a880702f0d23e9955a06f2)) - Maciej Spiechowicz
- (**diagnostics**) add scoped expiring capture sessions (#270) - ([489ec12](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/489ec12fdf7ceb87637a4b7b5a666a7b44d04b44)) - Maciej Spiechowicz
- (**ocpp**) add bounded multipart report collection (#279) - ([7d127e1](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/7d127e1fd284a482933bdf9d8e86cc7d9c4d2775)) - Maciej Spiechowicz
- (**protocol**) persist OCPP 1.6 transaction lifecycle (#277) - ([3d2137a](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/3d2137a8fb21a76b7fce8abbf4133dc6a31fa010)) - Maciej Spiechowicz
- (**protocol**) implement OCPP 2.0.1 charger authorization (#273) - ([dece52a](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/dece52a2bff8ca2460c924ff56a34b2f85552413)) - Maciej Spiechowicz
- (**protocol**) persist OCPP 2.0.1 registration and status (#272) - ([2f6eb8e](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/2f6eb8ecefa71793331f0d569d13909a34c72303)) - Maciej Spiechowicz
- (**protocol**) persist OCPP 1.6 registration and status (#271) - ([7435d71](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/7435d71d123fa00a8eb67c44915ba7a0590bed20)) - Maciej Spiechowicz
- (**release**) gate promotion on a race-safe idle drain (#278) - ([31e7246](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/31e7246a010c3b9e43f15ddfd79f7eecd0386e99)) - Maciej Spiechowicz
- (**release**) persist rollback signal classification (#269) - ([354cb19](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/354cb19115142e8ebe46dedcded1ba32de638ec0)) - Maciej Spiechowicz
- (**release**) validate production preflight and back up SQLite (#268) - ([6d95dab](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/6d95dabe680ad06cc718c9ee4b08694dfaa4c74b)) - Maciej Spiechowicz
- (**release**) verify trusted candidate qualification evidence (#267) - ([7b8b5e0](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/7b8b5e066b325cf92f219cabcc878bf5ea9d9ac6)) - Maciej Spiechowicz
- (**release**) persist recoverable activation transitions (#266) - ([8d9ce9b](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/8d9ce9bacdb3f8632a05830d20b6d9c08824382e)) - Maciej Spiechowicz
#### Continuous Integration
- upgrade release token action to Node 24 (#265) - ([ed71799](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/ed717999f4be4f380c9d93ee346040fe54e1a34d)) - Maciej Spiechowicz

- - -

## [v0.27.1](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/a5d69aac8ee32fb9dc0e48d8cb36700203c2228d..v0.27.1) - 2026-09-07
#### Bug Fixes
- (**release**) publish version bumps after one stable approval (#264) - ([a5d69aa](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/a5d69aac8ee32fb9dc0e48d8cb36700203c2228d)) - Maciej Spiechowicz

- - -

## [v0.27.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/391432e450bac38ee5b4e3075fd642127a49ce1e..v0.27.0) - 2026-09-07
#### Features
- (**release**) verify and retain signed application bundles (#261) - ([391432e](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/391432e450bac38ee5b4e3075fd642127a49ce1e)) - Maciej Spiechowicz

- - -

## [v0.26.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/ff57b13eddd0cad094fda424c1675800b9bec925..v0.26.0) - 2026-09-07
#### Features
- (**ems-scada**) publish the versioned OpenAPI contract (#251) - ([61968ed](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/61968ed7c65ff1e3d31a0b483f6986757e2c7ef6)) - Maciej Spiechowicz
- (**ems-scada**) expose resumable event subscriptions (#250) - ([b55c1f1](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/b55c1f1ae818fdf647d06c16bb704171fe08946f)) - Maciej Spiechowicz
- (**ems-scada**) expose command admission and status (#249) - ([93dc303](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/93dc303db9f490e9d7ba8fedffbb31bd60134ab1)) - Maciej Spiechowicz
- (**ems-scada**) expose bounded station and point queries (#248) - ([4589cbf](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/4589cbf29f2ba4d61f979991c91135e80e4acc08)) - Maciej Spiechowicz, Claude Opus 5
- (**ems-scada**) add the direct HTTP integration listener (#247) - ([c02ee55](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/c02ee55cf0f8b44215c11ddd674246f9bd9bd79f)) - Maciej Spiechowicz, Claude Opus 5
- (**mqtt**) add the EMS/SCADA MQTT preset (#246) - ([3f7b04e](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/3f7b04ed992f17c9abb62d31e45655e87124d149)) - Maciej Spiechowicz, Claude Opus 5
- (**ops**) reserve isolated staging disk capacity (#257) - ([b863b69](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/b863b69b30ea65c58664fc58fd9b6d67dd6d49ca)) - Maciej Spiechowicz
- (**ops**) report service readiness and worker-backed watchdog progress (#256) - ([fa4c285](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/fa4c285e493a9d9aa10bb091d0bdee8b99cfe433)) - Maciej Spiechowicz
- (**ops**) govern staging resource admission and shedding (#255) - ([a269dc7](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/a269dc7f02775e854714f22390d85aec719d1cd5)) - Maciej Spiechowicz
- (**ops**) isolate staging network access to test peers (#254) - ([304f23c](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/304f23c8692bcd60322b1c25c66bb037a74a8964)) - Maciej Spiechowicz
- (**ops**) isolate production and staging filesystems (#253) - ([f6cfd5a](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/f6cfd5ad6b39954940f6b662361d8d57bcafcbe9)) - Maciej Spiechowicz
- (**ops**) package non-root service and bound task shutdown (#252) - ([1a2a1ad](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/1a2a1ada366696090cd0129560833984d70dfdf4)) - Maciej Spiechowicz
- (**protocol**) authorize OCPP 1.6 charging identities (#245) - ([888c2ff](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/888c2ff0c4277eef21fb9b0c432f654f03819007)) - Maciej Spiechowicz
#### Bug Fixes
- (**ci**) publish releases from reviewed main versions - ([7a054e1](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/7a054e1ada1504c32c842a63e70c7e1a58bd3459)) - Maciej Spiechowicz
- (**ci**) query release merge settings through GraphQL (#259) - ([cfd1065](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/cfd1065330c8f8e829276f1fffd2c27e08b85c22)) - Maciej Spiechowicz
- (**ci**) bootstrap release dependencies on clean runners (#258) - ([607059e](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/607059e310e9808b69f6b227840e8579809d0870)) - Maciej Spiechowicz
#### Continuous Integration
- (**release**) enforce protected publication boundaries (#244) - ([ff57b13](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/ff57b13eddd0cad094fda424c1675800b9bec925)) - Maciej Spiechowicz

- - -

## [v0.25.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/8f34a19625d0230cfe824cbd79ffee1927103ce8..v0.25.0) - 2026-09-04
#### Features
- (**mqtt**) publish Home Assistant discovery - ([8f34a19](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/8f34a19625d0230cfe824cbd79ffee1927103ce8)) - Maciej Spiechowicz

- - -

## [v0.24.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/b4139afde2b36168a1c45ed57e3e29ce27351c77..v0.24.0) - 2026-09-04
#### Features
- (**protocol**) ingest OCPP 1.6J meter values - ([b4139af](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/b4139afde2b36168a1c45ed57e3e29ce27351c77)) - Maciej Spiechowicz

- - -

## [v0.23.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/4614a17bbcfc9c7d30dd9281a8ecc8166f63a58d..v0.23.0) - 2026-09-04
#### Features
- (**mqtt**) add authenticated command ingress - ([4614a17](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/4614a17bbcfc9c7d30dd9281a8ecc8166f63a58d)) - Maciej Spiechowicz

- - -

## [v0.22.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/3ce285ba06a46a82f8e8b2cad768da87a3aa9094..v0.22.0) - 2026-09-03
#### Features
- (**api**) stream authenticated resumable events - ([3ce285b](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/3ce285ba06a46a82f8e8b2cad768da87a3aa9094)) - Maciej Spiechowicz
- (**mqtt**) publish canonical outbound messages - ([994a2f4](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/994a2f4c6092a30985bd01909e58ec0af49d3f82)) - Maciej Spiechowicz

- - -

## [v0.21.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/e061794938ea5ba262500e57c515b7f66bae99c7..v0.21.0) - 2026-09-03
#### Features
- (**api**) expose durable command admission - ([e061794](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/e061794938ea5ba262500e57c515b7f66bae99c7)) - Maciej Spiechowicz

- - -

## [v0.20.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/484ba1e6b33be18201dab7f6482242fc193f9edd..v0.20.0) - 2026-09-03
#### Features
- (**protocol**) implement OCPP 2.0.1 transaction lifecycle - ([484ba1e](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/484ba1e6b33be18201dab7f6482242fc193f9edd)) - Maciej Spiechowicz

- - -

## [v0.19.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/e3ba3cec8df5435337c819dd249df447d33e0e3b..v0.19.0) - 2026-09-03
#### Features
- (**api**) expose canonical management reads - ([e3ba3ce](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/e3ba3cec8df5435337c819dd249df447d33e0e3b)) - Maciej Spiechowicz

- - -

## [v0.18.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/011b8ee710de86ce19d433ef2b19ac6b74883af3..v0.18.0) - 2026-09-03
#### Features
- (**protocol**) ingest OCPP 2.0.1 meter values - ([011b8ee](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/011b8ee710de86ce19d433ef2b19ac6b74883af3)) - Maciej Spiechowicz

- - -

## [v0.17.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/d502429c56fd3753b101f20e9218599dfef54d49..v0.17.0) - 2026-09-03
#### Features
- (**cli**) add noninteractive service commands - ([23eb95f](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/23eb95fee13810b534809b8c8bbdcd4bf472f8f7)) - Maciej Spiechowicz
#### Tests
- (**sim**) add authorization failure scenarios - ([d502429](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/d502429c56fd3753b101f20e9218599dfef54d49)) - Maciej Spiechowicz

- - -

## [v0.16.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/90c8dd2bfca66695be4b4ab47fb175f0d70e544f..v0.16.0) - 2026-09-03
#### Features
- (**sim**) add OCPP 2.0.1 charging scenarios - ([90c8dd2](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/90c8dd2bfca66695be4b4ab47fb175f0d70e544f)) - Maciej Spiechowicz

- - -

## [v0.15.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/4ae4c3b57d1440fdd3dff54b061b79e043653487..v0.15.0) - 2026-09-02
#### Features
- (**sim**) add OCPP 1.6 charging scenarios - ([4ae4c3b](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/4ae4c3b57d1440fdd3dff54b061b79e043653487)) - Maciej Spiechowicz

- - -

## [v0.14.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/199b93a2db33d31189923970735b870d1c203864..v0.14.0) - 2026-09-02
#### Features
- (**auth**) add durable local authorization policy - ([199b93a](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/199b93a2db33d31189923970735b870d1c203864)) - Maciej Spiechowicz

- - -

## [v0.13.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/417a1b9b82b8f68821abe9c4941b05e4215fa321..v0.13.0) - 2026-09-02
#### Features
- (**protocol**) correlate OCPP call lifecycles - ([417a1b9](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/417a1b9b82b8f68821abe9c4941b05e4215fa321)) - Maciej Spiechowicz

- - -

## [v0.12.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/d12bf51bfc773151f79a2282535981fad98212ae..v0.12.0) - 2026-09-02
#### Features
- (**protocol**) admit authenticated OCPP WebSockets - ([d12bf51](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/d12bf51bfc773151f79a2282535981fad98212ae)) - Maciej Spiechowicz

- - -

## [v0.11.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/12c9be96edb5ef061ee59c9a21e3b3ebbcaf5d36..v0.11.0) - 2026-09-02
#### Features
- (**operations**) expose core readiness and resource metrics - ([12c9be9](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/12c9be96edb5ef061ee59c9a21e3b3ebbcaf5d36)) - Maciej Spiechowicz

- - -

## [v0.10.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/0d495b9fb93b4742fad8f11dc0e58c7406e9543c..v0.10.0) - 2026-09-02
#### Features
- (**management**) enforce scoped remote access - ([0d495b9](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/0d495b9fb93b4742fad8f11dc0e58c7406e9543c)) - Maciej Spiechowicz

- - -

## [v0.9.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/7d55a2223c68a29c4d875de8b833cf279f1add40..v0.9.0) - 2026-09-02
#### Features
- (**protocol**) authenticate station transports - ([7d55a22](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/7d55a2223c68a29c4d875de8b833cf279f1add40)) - Maciej Spiechowicz

- - -

## [v0.8.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/5a59ef6549d06265d810aaf70272898e931d6b51..v0.8.0) - 2026-09-02
#### Features
- (**storage**) reserve journal capacity for active sessions - ([5a59ef6](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/5a59ef6549d06265d810aaf70272898e931d6b51)) - Maciej Spiechowicz

- - -

## [v0.7.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/ef34843841c3d7e940f66c07bb8c00f643dcbae1..v0.7.0) - 2026-09-02
#### Features
- (**storage**) resume durable event streams by cursor - ([ef34843](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/ef34843841c3d7e940f66c07bb8c00f643dcbae1)) - Maciej Spiechowicz

- - -

## [v0.6.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/ad82b2b10e1c4106c44f9c82b72da8add91f348c..v0.6.0) - 2026-09-02
#### Features
- (**application**) preserve uncertain command outcomes - ([ad82b2b](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/ad82b2b10e1c4106c44f9c82b72da8add91f348c)) - Maciej Spiechowicz

- - -

## [v0.5.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/0a1602e71f0fa860bb688801d9a1ec20c986d3c7..v0.5.0) - 2026-09-02
#### Features
- (**storage**) deduplicate commands for seven days - ([0a1602e](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/0a1602e71f0fa860bb688801d9a1ec20c986d3c7)) - Maciej Spiechowicz

- - -

## [v0.4.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/165fa7e18026a0ddaf975d22c79b64611b0e53e3..v0.4.0) - 2026-09-02
#### Features
- (**target**) dispatch durable deliveries - ([165fa7e](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/165fa7e18026a0ddaf975d22c79b64611b0e53e3)) - Maciej Spiechowicz

- - -

## [v0.3.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/e47de341fb4151daa823216027d62e7bebab63e0..v0.3.0) - 2026-09-02
#### Features
- (**target**) supervise bounded target sessions - ([e47de34](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/e47de341fb4151daa823216027d62e7bebab63e0)) - Maciej Spiechowicz

- - -

## [v0.2.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/b56e5020197bb7566f123ec9f0bfacf9df5873da..v0.2.0) - 2026-09-01
#### Features
- (**target**) guard destination changes - ([b56e502](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/b56e5020197bb7566f123ec9f0bfacf9df5873da)) - Maciej Spiechowicz

- - -

## [v0.1.0](https://github.com/MSpiechowicz/universal-ocpp-bridge/compare/1f30044b40cb4e8d022526d26c89db6923ac65b1..v0.1.0) - 2026-09-01
#### Features
- (**application**) centralize diagnostic redaction - ([ebb93bb](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/ebb93bb516d9c3eb882fbf1dcc918cf451d37c8c)) - Maciej Spiechowicz
- (**application**) enforce shared runtime budgets - ([cc8a99d](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/cc8a99dd90e81ab37b6cc911ed1f998dfff97b9d)) - Maciej Spiechowicz
- (**application**) define database provider contracts - ([cfe97e3](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/cfe97e3da43e6e347fe89b4b26a0bcddd7d8472f)) - Maciej Spiechowicz
- (**application**) implement scoped target queries - ([82a22d2](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/82a22d2390baaeb0c16b19c339e94a9bc0a22127)) - Maciej Spiechowicz
- (**application**) define bridge target contracts - ([872bb9d](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/872bb9dd5ee5fc1e734cf2122e8c2b34fd9b2d76)) - Maciej Spiechowicz
- (**application**) define operational store ports - ([e316c8f](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/e316c8f88fb0f3bc0b3a242b137440e75f75a74a)) - Maciej Spiechowicz
- (**architecture**) establish modular Rust workspace - ([b5343a4](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/b5343a4f287b1aa9a3bdecd3d1d5220231ae1adf)) - Maciej Spiechowicz
- (**contracts**) define external export records - ([bfc8b0b](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/bfc8b0baee8306381682d96f0e13c2488ca5bbe1)) - Maciej Spiechowicz
- (**contracts**) publish versioned JSON schemas - ([067906b](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/067906b71cd46d3f09e19081bba12105486e5e2c)) - Maciej Spiechowicz
- (**contracts**) define command lifecycle contracts - ([c03b883](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/c03b883d14575d807dba4bec5cc34154174025b2)) - Maciej Spiechowicz
- (**contracts**) define station snapshots - ([976dcf5](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/976dcf575e0382da42f2ab7681fa8b9e9c71f309)) - Maciej Spiechowicz
- (**contracts**) define typed data points - ([ff0eadb](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/ff0eadb58b2b7302b908a58c10138cf63abc666e)) - Maciej Spiechowicz
- (**contracts**) define canonical identities and events - ([051c6af](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/051c6afb180e957bfde5829520148f05f04ccb8a)) - Maciej Spiechowicz
- (**export**) validate optional provider destinations - ([1c816c4](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/1c816c4dd9b693204fa53cb2b2696e6e966e42be)) - Maciej Spiechowicz
- (**payment**) define provider orchestration boundary - ([e3e95d3](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/e3e95d36d8ac999ef5d487b39a8d595c3ac2e091)) - Maciej Spiechowicz
- (**protocol**) isolate pinned OCPP model adapters - ([fe44243](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/fe44243aac1f4f56600e6510fa0e49c038c516d2)) - Maciej Spiechowicz
- (**protocol**) supervise bounded station tasks - ([49f7e88](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/49f7e88353b541099147e90a3d6db199d05e7dd1)) - Maciej Spiechowicz
- (**release**) gate backward-compatible promotion - ([ef0bff2](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/ef0bff24e646c02d36f35bafaa625b710835b20b)) - Maciej Spiechowicz
- (**service**) construct trusted runtime identity - ([f4b3b26](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/f4b3b26792d312164497329adda61c50a8f60ec4)) - Maciej Spiechowicz
- (**sim**) isolate multi-station scheduling - ([af007cd](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/af007cdb81d5571b93819be49b7b932758308045)) - Maciej Spiechowicz
- (**sim**) add deterministic scenario runner - ([f045cc3](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/f045cc39e91c40a47de18a61efa324ce473ab852)) - Maciej Spiechowicz
- (**sim**) validate dual-version OCPP client - ([b57da71](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/b57da71d0f5897b98858127d42ba1d5b33ee31ff)) - Maciej Spiechowicz
- (**storage**) implement atomic SQLite operational store - ([2dda07d](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/2dda07d55bac8ce8bf2663d41f400770e256d16f)) - Maciej Spiechowicz
- (**target**) reserve industrial adapter extension boundary - ([6e5bd0a](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/6e5bd0ad201fabf9dcba31c86e338d12548e3ad4)) - Maciej Spiechowicz
- (**target**) validate explicit target selection - ([28ba688](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/28ba6883126f562fe6d367d2ba0a16e8b84dbf19)) - Maciej Spiechowicz
#### Bug Fixes
- (**ci**) configure Cocogitto identity - ([d6918c8](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/d6918c81321e31b34292db221ca2f10050710863)) - Maciej Spiechowicz
- (**ci**) install verified Cocogitto binary - ([364969e](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/364969edc85482b06798ce19d126a89f2a0db968)) - Maciej Spiechowicz
- (**tooling**) verify workspace without local Cargo - ([ae6f2c0](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/ae6f2c0ad0f89fc7155544eb836fbf8be6eabe3b)) - Maciej Spiechowicz
#### Documentation
- move architecture plan into .agents - ([4fc828d](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/4fc828daa9bb93c441c6d9f243855482dbef77fc)) - Maciej Spiechowicz
- record OCPP bridge architecture and delivery plan - ([376b49a](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/376b49a49db111ed55300733054c6c6656db09e5)) - Maciej Spiechowicz
#### Tests
- (**export**) add database provider conformance suite - ([8005226](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/8005226b8d2b0a2df43515e2d08d83febbebe4ea)) - Maciej Spiechowicz
- (**ocpp**) establish independent fixture corpus - ([dcb17bc](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/dcb17bcde06bfd5a8b0a5e7acbb579997ed7352b)) - Maciej Spiechowicz
- (**sim**) add adversarial websocket peer - ([113c535](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/113c5359a8f909d1af944b43119a2df6ae939b48)) - Maciej Spiechowicz
- (**target**) add reusable conformance harness - ([ec4e18b](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/ec4e18b25e80575aa9ce6f0114ebcf143f311a81)) - Maciej Spiechowicz
#### Build system
- (**deps**) refresh Rust dependencies - ([5603ed5](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/5603ed5765dc5ae37693cb2f74b758d50b4b783a)) - Maciej Spiechowicz
#### Continuous Integration
- (**release**) automate semantic version releases - ([2e9a012](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/2e9a0124d135b7d3b44d501a9588f255fd916af1)) - Maciej Spiechowicz
- (**security**) add dependency and workflow gates - ([dd76213](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/dd76213df0f13d0c5ab25582848a8488625dd730)) - Maciej Spiechowicz
- optimize checks and update Rust toolchain - ([d02e4b2](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/d02e4b217d64e5711b9fea6a18c4b9a0da3efd47)) - Maciej Spiechowicz
- add safe Cocogitto checks - ([a9171c9](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/a9171c9f8001891e03504ade43bca6d5fe54d7c7)) - Maciej Spiechowicz
- enforce pinned Rust repository checks - ([1d11620](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/1d11620964b3f976fd34b4b4e89857bc2ede9539)) - Maciej Spiechowicz
#### Refactoring
- enforce maintainable file sizes - ([543aa40](https://github.com/MSpiechowicz/universal-ocpp-bridge/commit/543aa40ea8cefeeef78032cdc9fc51329420463f)) - Maciej Spiechowicz

- - -

Changelog generated by [cocogitto](https://github.com/cocogitto/cocogitto).