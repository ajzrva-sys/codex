<p align="center"><strong>Codex CLI</strong> is a coding agent from OpenAI that runs locally on your computer.
<p align="center">
  <img src="https://github.com/openai/codex/blob/main/.github/codex-cli-splash.png" alt="Codex CLI splash" width="80%" />
</p>
</br>
If you want Codex in your code editor (VS Code, Cursor, Windsurf), <a href="https://developers.openai.com/codex/ide">install in your IDE.</a>
</br>If you want the desktop app experience, run <code>codex app</code> or visit <a href="https://chatgpt.com/codex?app-landing-page=true">the Codex App page</a>.
</br>If you are looking for the <em>cloud-based agent</em> from OpenAI, <strong>Codex Web</strong>, go to <a href="https://chatgpt.com/codex">chatgpt.com/codex</a>.</p>

---

## Quickstart

**FreeBSD:** follow [Installing on FreeBSD](#installing-on-freebsd) below to build this fork and enable its native jail sandbox.

### Installing and running Codex CLI

Run the following on Mac or Linux to install Codex CLI:

```shell
curl -fsSL https://chatgpt.com/codex/install.sh | sh
```

Run the following on Windows to install Codex CLI:

```shell
powershell -ExecutionPolicy ByPass -c "irm https://chatgpt.com/codex/install.ps1 | iex"
```

The standalone installers download from `https://releases.openai.com/codex` by default and fall back to GitHub Releases if a metadata or asset download is unavailable. To force GitHub Releases, set `CODEX_INSTALLER_USE_RELEASES_OPENAI_COM` to `false` (`0` and `no` are also accepted):

```shell
curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_INSTALLER_USE_RELEASES_OPENAI_COM=false sh
```

```powershell
$env:CODEX_INSTALLER_USE_RELEASES_OPENAI_COM='false'; irm https://chatgpt.com/codex/install.ps1 | iex
```

Codex CLI can also be installed via the following package managers:

```shell
# Install using npm
npm install -g @openai/codex
```

```shell
# Install using Homebrew
brew install --cask codex
```

Then simply run `codex` to get started.

<details>
<summary>You can also go to the <a href="https://github.com/openai/codex/releases/latest">latest GitHub Release</a> and download the appropriate binary for your platform.</summary>

Each GitHub Release contains many executables, but in practice, you likely want one of these:

- macOS
  - Apple Silicon/arm64: `codex-aarch64-apple-darwin.tar.gz`
  - x86_64 (older Mac hardware): `codex-x86_64-apple-darwin.tar.gz`
- Linux
  - x86_64: `codex-x86_64-unknown-linux-musl.tar.gz`
  - arm64: `codex-aarch64-unknown-linux-musl.tar.gz`

Each archive contains a single entry with the platform baked into the name (e.g., `codex-x86_64-unknown-linux-musl`), so you likely want to rename it to `codex` after extracting it.

</details>

### Installing on FreeBSD

This fork supports native **FreeBSD 15.1 amd64**, including jail sandboxing for tool commands. Build and install the local npm package below; the official npm package does not include this FreeBSD build.

**1. Install build dependencies as root:**

```sh
pkg install git rust cmake gmake pkgconf protobuf python3 node24 npm \
  ripgrep bash alsa-lib dbus oniguruma gn ninja llvm21 glib
```

**2. Clone and build as your regular user:**

```sh
git clone -b main https://github.com/ajzrva-sys/codex.git
cd codex
python3 scripts/freebsd/package.py
```

The first build also compiles the V8 code-mode helper from source. For a faster development build, add `--profile dev-small`.

**3. Install the package and sandbox service as root:**

Replace the tarball path with your build's absolute path and `YOUR_USERNAME` with the ordinary account that will run Codex.

```sh
npm install -g /absolute/path/to/codex/dist/freebsd/openai-codex-0.0.0-freebsd.tgz
python3 "$(npm root -g)/@openai/codex/freebsd/install_sandbox.py" \
  --user YOUR_USERNAME \
  --daemon "$(npm root -g)/@openai/codex/vendor/x86_64-unknown-freebsd/bin/codex-freebsd-sandboxd"
```

The administrator installer enables a root-owned service that creates jails for the allowed user. Installing the npm package alone does not enable that service.

**4. Configure, sign in, and run as your regular user:**

```sh
python3 "$(npm root -g)/@openai/codex/freebsd/configure_sandbox.py"
codex login --device-auth
cd /path/to/project
codex doctor
codex sandbox -- /bin/sh -c 'id; sysctl security.jail.jailed'
codex -a on-request
```

The `freebsd-workspace` profile allows project writes and runtime reads, with networking disabled for tool commands. Codex and its authenticated connection run as your regular user outside the jail; tools run as that same user inside it, without access to host credentials. An unavailable sandbox service blocks sandboxed commands.

See the [FreeBSD build and sandbox guide](./scripts/freebsd/README.md) for additional path grants, networking, validation, limitations, and rollback.

### Using Codex with your ChatGPT plan

Run `codex` and select **Sign in with ChatGPT**. We recommend signing into your ChatGPT account to use Codex as part of your Plus, Pro, Business, Edu, or Enterprise plan. [Learn more about what's included in your ChatGPT plan](https://help.openai.com/en/articles/11369540-codex-in-chatgpt).

You can also use Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)
- [**FreeBSD build & sandbox setup**](./scripts/freebsd/README.md)
- [**Open source fund**](./docs/open-source-fund.md)

This repository is licensed under the [Apache-2.0 License](LICENSE).
