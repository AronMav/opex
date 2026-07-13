"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  useSidebar,
} from "@/components/ui/sidebar";
import { useAuthStore } from "@/stores/auth-store";
import { useWsStore } from "@/stores/ws-store";
import { useTranslation } from "@/hooks/use-translation";
import { Button } from "@/components/ui/button";
import { ModeToggle } from "@/components/mode-toggle";
import { LanguageToggle } from "@/components/language-toggle";
import { NotificationBell } from "@/components/notification-bell";
import {
  MessageSquare, Bot, FileText, Clock, Brain,
  Wrench, Folder, Key, Shield, Settings, LogOut, BookOpen,
  Radio, Zap, Webhook, Link2,
  Archive, Monitor,
  type LucideProps,
} from "lucide-react";
import { WalnutMark } from "@/components/ui/walnut-mark";
import type { TranslationKey } from "@/i18n/types";

interface NavItem {
  labelKey: TranslationKey;
  href: string;
  icon: React.FC<LucideProps>;
}

interface NavGroup {
  labelKey?: TranslationKey;
  items: NavItem[];
}

const NAV: NavGroup[] = [
  // Primary interaction surfaces — always visible, no label
  {
    items: [
      { labelKey: "nav.chat", href: "/chat/", icon: MessageSquare },
    ],
  },
  // Agent capabilities — what agents are, know, and can do
  {
    labelKey: "nav.agents_group",
    items: [
      { labelKey: "nav.agents", href: "/agents/", icon: Bot },
      { labelKey: "nav.skills", href: "/skills/", icon: BookOpen },
      { labelKey: "nav.tools", href: "/tools/", icon: Wrench },
      { labelKey: "nav.memory", href: "/memory/", icon: Brain },
      { labelKey: "nav.tasks", href: "/tasks/", icon: Clock },
      { labelKey: "nav.webhooks", href: "/webhooks/", icon: Webhook },
      { labelKey: "nav.integrations", href: "/integrations/", icon: Link2 },
    ],
  },
  // Monitoring — single consolidated page
  {
    labelKey: "nav.monitor",
    items: [
      { labelKey: "nav.monitor_single", href: "/monitor/", icon: Monitor },
    ],
  },
  // System administration — infrastructure, security, data
  {
    labelKey: "nav.system",
    items: [
      { labelKey: "nav.providers", href: "/providers/", icon: Zap },
      { labelKey: "nav.secrets", href: "/secrets/", icon: Key },
      { labelKey: "nav.config", href: "/config/", icon: Settings },
      { labelKey: "nav.channels", href: "/channels/", icon: Radio },
      { labelKey: "nav.access", href: "/access/", icon: Shield },
      { labelKey: "nav.files", href: "/workspace/", icon: Folder },
      { labelKey: "nav.backups", href: "/backups/", icon: Archive },
    ],
  },
];

export function AppSidebar() {
  const pathname = usePathname();
  const version = useAuthStore((s) => s.version);
  const logout = useAuthStore((s) => s.logout);
  const connected = useWsStore((s) => s.connected);
  const { t } = useTranslation();
  const { setOpenMobile, isMobile } = useSidebar();

  return (
    <Sidebar className="border-r border-border bg-sidebar">
      <SidebarHeader className="px-4 py-4 md:px-6 md:py-6">
        <div className="flex items-center gap-2">
          <WalnutMark className="text-primary" size={48} />
          <span className="font-display text-base font-bold tracking-wide text-foreground">
            OPEX
          </span>
          <span
            className={`ml-auto text-xs ${connected ? "text-success" : "text-destructive"}`}
          >
            {connected ? t("nav.online") : t("nav.offline")}
          </span>
        </div>
      </SidebarHeader>

      <SidebarContent className="px-3">
        {NAV.map((group, gi) => (
          <SidebarGroup key={gi} className="py-2">
            {group.labelKey && (
              <SidebarGroupLabel className="px-3 text-xs font-semibold uppercase tracking-wide text-muted-foreground-subtle">
                {t(group.labelKey)}
              </SidebarGroupLabel>
            )}
            <SidebarGroupContent>
              <SidebarMenu className="gap-1">
                {group.items.map((item) => (
                  <SidebarMenuItem key={item.href}>
                    <SidebarMenuButton
                      asChild
                      isActive={pathname.startsWith(item.href)}
                      className="group/nav relative h-10 md:h-10 px-3 transition-colors duration-150 hover:bg-accent active:bg-accent/50 active:scale-[0.99] overflow-hidden rounded-md"
                    >
                      <Link href={item.href} onClick={() => isMobile && setOpenMobile(false)} className="flex items-center gap-3 w-full">
                        <item.icon className={`transition-colors duration-150 ${pathname.startsWith(item.href) ? "text-primary" : "text-muted-foreground group-hover/nav:text-foreground"}`} />
                        <span className={`font-medium tracking-tight text-sm truncate min-w-0 transition-colors duration-150 ${pathname.startsWith(item.href) ? "text-foreground font-bold" : "text-muted-foreground group-hover/nav:text-foreground"}`}>
                          {t(item.labelKey)}
                        </span>

                        {/* Active Indicator - Left (Clean Line) */}
                        {pathname.startsWith(item.href) && (
                          <div className="absolute left-0 top-2 bottom-2 w-[3px] rounded-r-full bg-primary" />
                        )}

                        <div className="absolute inset-0 bg-primary/0 group-hover/nav:bg-primary/[0.03] transition-colors duration-150" />
                      </Link>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                ))}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        ))}
      </SidebarContent>

      <SidebarFooter className="border-t border-border bg-muted/30 p-3 md:p-4">
        <div className="flex items-center justify-between rounded-lg bg-muted/30 px-3 py-2.5">
          <div className="flex flex-col">
            <span className="text-xs text-muted-foreground-subtle">{t("common.version")}</span>
            <span className="font-mono text-xs font-bold text-muted-foreground">
              {version || "0.1.0-alpha"}
            </span>
          </div>
          <div className="flex items-center gap-1">
            <LanguageToggle />
            <ModeToggle />
            <NotificationBell />
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={() => {
                logout();
                window.location.href = "/login/";
              }}
              className="group text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
              title={t("nav.logout")}
              aria-label={t("nav.logout")}
            >
              <LogOut className="transition-transform active:translate-x-0.5" size={20} />
            </Button>
          </div>
        </div>
      </SidebarFooter>
    </Sidebar>
  );
}
