PREFIX ?= $(HOME)/.local
DESTDIR ?=
BINDIR ?= $(PREFIX)/bin
XDG_DATA_HOME ?=
EFFECTIVE_XDG_DATA_HOME := $(if $(strip $(XDG_DATA_HOME)),$(XDG_DATA_HOME),$(HOME)/.local/share)
SYSTEMD_USER_DIR ?= $(if $(filter $(HOME)/.local,$(PREFIX)),$(EFFECTIVE_XDG_DATA_HOME)/systemd/user,$(PREFIX)/share/systemd/user)
MANDIR ?= $(PREFIX)/share/man
DOCDIR ?= $(PREFIX)/share/doc/mural
PROFILE ?= release
CARGO ?= cargo
CARGO_FLAGS ?= --locked
CARGO_TARGET_DIR ?= target
INSTALL ?= install
SYSTEMCTL ?= systemctl
SED ?= sed

SYSTEMD_UNIT := $(CARGO_TARGET_DIR)/murald.service

ifeq ($(PROFILE),release)
CARGO_PROFILE_FLAG := --release
TARGET_DIR := $(CARGO_TARGET_DIR)/release
else
CARGO_PROFILE_FLAG :=
TARGET_DIR := $(CARGO_TARGET_DIR)/debug
endif

.PHONY: all build check test clippy install uninstall install-service \
	uninstall-service reload-service enable-service disable-service \
	restart-service status clean FORCE

all: build

build:
	$(CARGO) build $(CARGO_PROFILE_FLAG) $(CARGO_FLAGS)

check:
	$(CARGO) check $(CARGO_FLAGS)

test:
	$(CARGO) test $(CARGO_FLAGS)

clippy:
	$(CARGO) clippy --all-targets --all-features $(CARGO_FLAGS) -- -D warnings

$(SYSTEMD_UNIT): dist/systemd/murald.service.in Makefile FORCE
	mkdir -p $(dir $@)
	$(SED) 's|@BINDIR@|$(BINDIR)|g' $< > $@

FORCE:

install: build install-service
	$(INSTALL) -Dm755 $(TARGET_DIR)/murald $(DESTDIR)$(BINDIR)/murald
	$(INSTALL) -Dm755 $(TARGET_DIR)/muralctl $(DESTDIR)$(BINDIR)/muralctl
	$(INSTALL) -Dm644 docs/man/mural.7 $(DESTDIR)$(MANDIR)/man7/mural.7
	$(INSTALL) -Dm644 docs/man/murald.1 $(DESTDIR)$(MANDIR)/man1/murald.1
	$(INSTALL) -Dm644 docs/man/muralctl.1 $(DESTDIR)$(MANDIR)/man1/muralctl.1
	$(INSTALL) -Dm644 docs/man/mural-config.5 $(DESTDIR)$(MANDIR)/man5/mural-config.5
	$(INSTALL) -Dm644 examples/config $(DESTDIR)$(DOCDIR)/examples/config
	$(INSTALL) -Dm644 LICENSE-APACHE $(DESTDIR)$(PREFIX)/share/licenses/mural/LICENSE-APACHE
	$(INSTALL) -Dm644 LICENSE-MIT $(DESTDIR)$(PREFIX)/share/licenses/mural/LICENSE-MIT
	@printf '%s\n' 'Installed murald, muralctl, manuals, sample config, licenses, and murald.service.'
	@printf '%s\n' 'For a live user install, run `make enable-service`.'

uninstall: uninstall-service
	rm -f $(DESTDIR)$(BINDIR)/murald $(DESTDIR)$(BINDIR)/muralctl
	rm -f $(DESTDIR)$(MANDIR)/man7/mural.7 $(DESTDIR)$(MANDIR)/man1/murald.1 $(DESTDIR)$(MANDIR)/man1/muralctl.1 $(DESTDIR)$(MANDIR)/man5/mural-config.5
	rm -f $(DESTDIR)$(DOCDIR)/examples/config
	rm -f $(DESTDIR)$(PREFIX)/share/licenses/mural/LICENSE-APACHE $(DESTDIR)$(PREFIX)/share/licenses/mural/LICENSE-MIT

install-service: $(SYSTEMD_UNIT)
	$(INSTALL) -Dm644 $(SYSTEMD_UNIT) $(DESTDIR)$(SYSTEMD_USER_DIR)/murald.service

uninstall-service:
	rm -f $(DESTDIR)$(SYSTEMD_USER_DIR)/murald.service

reload-service:
	$(SYSTEMCTL) --user daemon-reload

enable-service:
	$(SYSTEMCTL) --user daemon-reload
	$(SYSTEMCTL) --user enable --now murald.service

disable-service:
	-$(SYSTEMCTL) --user disable --now murald.service

restart-service:
	$(SYSTEMCTL) --user restart murald.service

status:
	$(SYSTEMCTL) --user status murald.service

clean:
	$(CARGO) clean
