"use strict";

const express = require("express");
const controller = require("../controllers/eventsController");

const router = express.Router();
router.get("/stream", controller.stream);

module.exports = router;
