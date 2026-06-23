"use client";

import { useEffect } from "react";
import { useRouter } from "next/navigation";
import { useAuthStore } from "@/stores/auth-store";
import { CircularLoader } from "@/components/ui/loader";

export default function RootPage() {
  const router = useRouter();
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated);
  const restore = useAuthStore((s) => s.restore);

  useEffect(() => {
    if (isAuthenticated) {
      router.replace("/chat");
      return;
    }
    restore().then((ok) => {
      router.replace(ok ? "/chat" : "/login");
    });
  }, [isAuthenticated, restore, router]);

  return (
    <div className="flex h-dvh items-center justify-center">
      <CircularLoader size="md" />
    </div>
  );
}
