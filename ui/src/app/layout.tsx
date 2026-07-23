import type { Metadata, Viewport } from "next";
import { Nunito, Manrope, JetBrains_Mono } from "next/font/google";
import { ThemeProvider } from "@/components/theme-provider";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { LanguageSync } from "@/components/language-sync";
import { SearchPalette } from "@/components/chat/SearchPalette";
import "./globals.css";

const nunito = Nunito({
  subsets: ["latin", "cyrillic"],
  weight: ["300", "400", "500", "600", "700", "800"],
  variable: "--font-display",
});

const manrope = Manrope({
  subsets: ["latin", "cyrillic"],
  variable: "--font-sans",
});

const jetbrains = JetBrains_Mono({
  subsets: ["latin", "cyrillic"],
  variable: "--font-mono",
});

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  interactiveWidget: "resizes-content",
};

export const metadata: Metadata = {
  title: "OPEX",
  description: "AI Gateway Control Panel",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  // NOTE: the app is a static export (`output: "export"`), so `next/headers`
  // cookies() is unavailable at build time. The <html lang> is corrected on the
  // client after hydration by <LanguageSync> based on the persisted locale.
  return (
    <html lang="en" suppressHydrationWarning>
      <body className={`${nunito.variable} ${manrope.variable} ${jetbrains.variable} font-sans antialiased h-[100dvh] overflow-hidden`}>
        <ThemeProvider
          attribute="class"
          defaultTheme="system"
          enableSystem
          disableTransitionOnChange
        >
          <TooltipProvider>
            {children}
          </TooltipProvider>
          <Toaster position="top-center" />
          <SearchPalette />
          <LanguageSync />
        </ThemeProvider>
      </body>
    </html>
  );
}
