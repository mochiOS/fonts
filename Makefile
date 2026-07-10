ROOT		?= .
OUT		?= $(ROOT)/out
FONT_OUT	?= $(OUT)/fonts
CACHE		?= $(OUT)/cache/fonts
WORK		?= $(OUT)/build/fonts

CURL		?= curl
PERL		?= perl

CONFIG		:= $(CURDIR)/fonts.conf
INSTALLER	:= $(CURDIR)/scripts/install-fonts.pl
STAMP		:= $(FONT_OUT)/.installed

.PHONY: all fonts clean distclean list

all: fonts

fonts: $(STAMP)

$(STAMP): $(CONFIG) $(INSTALLER)
	@mkdir -p $(FONT_OUT)
	@mkdir -p $(CACHE)
	@mkdir -p $(WORK)
	@$(PERL) $(INSTALLER) \
		--config $(CONFIG) \
		--output $(FONT_OUT) \
		--cache $(CACHE) \
		--work $(WORK) \
		--curl $(CURL)
	@touch $(STAMP)

list:
	@$(PERL) $(INSTALLER) \
		--config $(CONFIG) \
		--list

clean:
	@rm -rf $(FONT_OUT)
	@rm -rf $(WORK)

distclean: clean
	@rm -rf $(CACHE)