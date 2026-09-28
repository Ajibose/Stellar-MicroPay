/**
 * src/server.js
 * Express server entry point for Stellar MicroPay backend.
 */

"use strict";

const express = require("express");
const cors = require("cors");
const helmet = require("helmet");
const morgan = require("morgan");
const rateLimit = require("express-rate-limit");
require("dotenv").config();

const accountRoutes = require("./routes/accounts");
const authRoutes = require("./routes/auth");
const paymentRoutes = require("./routes/payments");
const analyticsRoutes = require("./routes/analytics");
const healthRoutes = require("./routes/health");
const federationRoutes = require("./routes/federation");
const turretsRoutes = require("./routes/turrets");
const tipsRoutes = require("./routes/tips");
const swaggerUi = require("swagger-ui-express");
const swaggerSpec = require("./swagger");
const { startTurretsServer } = require("./turretsServer");

const app = express();
const PORT = process.env.PORT || 4000;

// ─── Middleware ───────────────────────────────────────────────────────────────

app.set('trust proxy', true);

// Enforce HTTPS in production (redirect HTTP to HTTPS)
app.use((req, res, next) => {
  if (process.env.NODE_ENV === 'production' && req.headers['x-forwarded-proto'] !== 'https' && req.protocol !== 'https') {
    return res.redirect(`https://${req.get('host')}${req.originalUrl}`);
  }
  next();
});

// Remove the framework fingerprint header (helmet also does this, but disabling
// at the Express level guarantees it even if helmet config changes).
app.disable("x-powered-by");

/**
 * Content-Security-Policy directives for this JSON API.
 *
 * The backend serves no HTML pages of its own except Swagger UI at /api/docs,
 * so the policy is intentionally restrictive:
 *
 *  defaultSrc  – block everything not listed explicitly.
 *  scriptSrc   – only same-origin scripts (Swagger UI bundles its own JS).
 *  styleSrc    – same-origin + unsafe-inline (Swagger UI injects inline styles).
 *  imgSrc      – same-origin + data URIs (Swagger UI logo).
 *  connectSrc  – only same-origin fetch/XHR (all API calls go to self).
 *  fontSrc     – same-origin only.
 *  objectSrc   – none (no Flash / plugins).
 *  frameSrc    – none (not embedded in iframes).
 *  upgradeInsecureRequests – omitted intentionally; handled at the load-balancer
 *                            level in production.
 *
 * Helmet v7+ ships with CSP *disabled* by default, so this must be explicit.
 */
const helmetOptions = {
  contentSecurityPolicy: {
    directives: {
      defaultSrc: ["'self'"],
      scriptSrc: ["'self'"],
      styleSrc: ["'self'", "'unsafe-inline'"],
      imgSrc: ["'self'", "data:"],
      connectSrc: ["'self'"],
      fontSrc: ["'self'"],
      objectSrc: ["'none'"],
      frameSrc: ["'none'"],
      // Disallow this API from being framed by any site (clickjacking defence,
      // the CSP-level equivalent of X-Frame-Options: DENY).
      frameAncestors: ["'none'"],
      // Forbid <base> tag hijacking and form posts to third-party origins.
      baseUri: ["'self'"],
      formAction: ["'self'"],
    },
  },
  // HTTP Strict Transport Security — force HTTPS for two years, cover subdomains,
  // and allow browser-preload-list inclusion. TLS is terminated at the
  // load-balancer, so the header is emitted here for clients that reach us
  // directly over HTTPS.
  hsts: {
    maxAge: 63072000, // 2 years
    includeSubDomains: true,
    preload: true,
  },
  // Send no referrer to other origins (avoids leaking API paths / tokens in
  // Referer headers).
  referrerPolicy: { policy: "no-referrer" },
  // This JSON API should never be embedded cross-origin, nor share its window.
  crossOriginResourcePolicy: { policy: "same-site" },
  crossOriginOpenerPolicy: { policy: "same-origin" },
  // Belt-and-braces clickjacking header for older clients that ignore CSP.
  frameguard: { action: "deny" },
  // Block Adobe cross-domain policy files.
  permittedCrossDomainPolicies: { permittedPolicies: "none" },
};

app.use(helmet(helmetOptions));
app.use(morgan("dev"));
app.use(express.json({ limit: "10kb" }));

// JSON parsing error handler
app.use((err, req, res, next) => {
  if (err instanceof SyntaxError && err.status === 400 && "body" in err) {
    return res.status(400).json({ error: "Invalid JSON body" });
  }
  next();
});

// CORS
const allowedOrigins = (process.env.ALLOWED_ORIGINS || "http://localhost:3000")
  .split(",")
  .map((o) => o.trim());

app.use(
  cors({
    origin: (origin, callback) => {
      // Allow requests with no origin (e.g. curl, Postman)
      if (!origin || allowedOrigins.includes(origin)) {
        callback(null, true);
      } else {
        callback(new Error(`CORS: origin ${origin} not allowed`));
      }
    },
    methods: ["GET", "POST"],
    allowedHeaders: ["Content-Type", "Authorization"],
    credentials: true,
  })
);

// Global rate limiting — 100 requests per 15 minutes per IP
const limiter = rateLimit({
  windowMs: 15 * 60 * 1000,
  max: 100,
  standardHeaders: true,
  legacyHeaders: false,
  message: { error: "Too many requests, please try again later." },
});
app.use(limiter);

// ─── Routes ──────────────────────────────────────────────────────────────────

app.use("/api/auth", authRoutes);
app.use("/api/accounts", accountRoutes);
app.use("/api/payments", paymentRoutes);
app.use("/health", healthRoutes);
app.use("/api/analytics", analyticsRoutes);
app.use("/api/health", healthRoutes);
app.use("/api/turrets", turretsRoutes);
app.use("/api/tips", tipsRoutes);
app.use("/federation", federationRoutes);

// ─── API Documentation ─────────────────────────────────────────────────────────

app.use("/api/docs", swaggerUi.serve, swaggerUi.setup(swaggerSpec, {
  customSiteTitle: "Stellar MicroPay API Docs",
  customCss: ".swagger-ui .topbar { display: none }",
  swaggerOptions: { url: "/api/docs.json" },
}));

app.get("/api/docs.json", (req, res) => {
  res.setHeader("Content-Type", "application/json");
  res.send(swaggerSpec);
});

// ─── Error Handling ────────────────────────────────────────────────────────────

app.use((err, req, res, next) => {
  void next;
  const status = err.status || 500;
  const message = err.message || "Internal Server Error";

  res.status(status).json({ error: message });
});

// ─── Static Files ─────────────────────────────────────────────────────────────

app.get("/.well-known/stellar.toml", (req, res) => {
  const domain = process.env.DOMAIN || "stellarmicropay.com";
  const tomlContent = `[FEDERATION_SERVER]
ACTIVE = true
SERVER = "https://${domain}/federation"
`;
  res.setHeader("Content-Type", "application/toml");
  res.send(tomlContent);
});

// ─── Start ────────────────────────────────────────────────────────────────────

if (require.main === module) {
  app.listen(PORT, () => {
    console.log(`
  ✨ Stellar MicroPay API
  🚀 Server running at http://localhost:${PORT}
  🌐 Network: ${process.env.STELLAR_NETWORK || "testnet"}
  `);
  });

  startTurretsServer();
}

module.exports = app;
