import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
} from "react";

const RouterContext = createContext(null);
const OutletContext = createContext({});

function sanitizePath(to) {
  if (typeof to !== "string" || !to.startsWith("/") || to.startsWith("//")) {
    return "/";
  }
  if (to.includes("\\")) {
    return "/";
  }
  try {
    const url = new URL(to, window.location.origin);
    if (url.origin !== window.location.origin) {
      return "/";
    }
    return `${url.pathname}${url.search}${url.hash}`;
  } catch {
    return "/";
  }
}

export function BrowserRouter({ children }) {
  const [location, setLocation] = useState(() => ({
    pathname: window.location.pathname || "/",
    search: window.location.search || "",
    hash: window.location.hash || "",
    state: window.history.state?.usr || null,
  }));

  useEffect(() => {
    const update = () =>
      setLocation({
        pathname: window.location.pathname || "/",
        search: window.location.search || "",
        hash: window.location.hash || "",
        state: window.history.state?.usr || null,
      });
    window.addEventListener("popstate", update);
    return () => window.removeEventListener("popstate", update);
  }, []);

  const navigate = useCallback((to, options = {}) => {
    const path = sanitizePath(to);
    const state = options?.state || null;
    if (options?.replace) {
      window.history.replaceState({ usr: state }, "", path);
    } else {
      window.history.pushState({ usr: state }, "", path);
    }
    window.dispatchEvent(new PopStateEvent("popstate", { state: { usr: state } }));
  }, []);

  const value = useMemo(
    () => ({ location, navigate }),
    [location, navigate]
  );

  return (
    <RouterContext.Provider value={value}>{children}</RouterContext.Provider>
  );
}

export function useLocation() {
  const context = useContext(RouterContext);
  if (!context) {
    throw new Error("useLocation must be used within BrowserRouter");
  }
  return context.location;
}

export function useNavigate() {
  const context = useContext(RouterContext);
  if (!context) {
    throw new Error("useNavigate must be used within BrowserRouter");
  }
  return context.navigate;
}

export function Navigate({ to, replace = false, state = null }) {
  const navigate = useNavigate();
  useEffect(() => {
    navigate(to, { replace, state });
  }, [navigate, replace, state, to]);
  return null;
}

export function Link({ to, children, onClick, ...props }) {
  const navigate = useNavigate();
  const href = sanitizePath(to);
  const handleClick = (event) => {
    onClick?.(event);
    if (
      event.defaultPrevented ||
      event.button !== 0 ||
      event.metaKey ||
      event.altKey ||
      event.ctrlKey ||
      event.shiftKey
    ) {
      return;
    }
    event.preventDefault();
    navigate(href);
  };
  return (
    <a href={href} onClick={handleClick} {...props}>
      {children}
    </a>
  );
}

export function NavLink({ to, children, className, end = false, ...props }) {
  const location = useLocation();
  const href = sanitizePath(to);
  const isActive = end
    ? location.pathname === href
    : location.pathname === href || location.pathname.startsWith(`${href}/`);
  const resolvedClassName =
    typeof className === "function"
      ? className({ isActive, isPending: false })
      : className;
  return (
    <Link to={href} className={resolvedClassName} {...props}>
      {typeof children === "function"
        ? children({ isActive, isPending: false })
        : children}
    </Link>
  );
}

export function Outlet({ context = {}, children = null }) {
  return (
    <OutletContext.Provider value={context}>{children}</OutletContext.Provider>
  );
}

export function useOutletContext() {
  return useContext(OutletContext);
}
